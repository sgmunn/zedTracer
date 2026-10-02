//! The history of runs: a picker to show an earlier result again, and the deletion of the oldest
//! result files so the history folder does not grow without end.
//!
//! The list comes from the run log rather than from the result files, which can be tens of
//! megabytes each.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, TimeZone};
use fs::{Fs, RemoveOptions};
use futures::StreamExt as _;
use gpui::{
    Action as _, App, AsyncWindowContext, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Task, WeakEntity, actions,
};
use gpui_util::ResultExt as _;
use kusto_client::{
    HistoryEntry, HistoryFile, HistoryLimits, HistoryOutcome, RUN_LOG_FILE, files_to_prune,
    history_entries,
};
use picker::{Picker, PickerDelegate};
use settings::Settings as _;
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
use workspace::notifications::DetachAndPromptErr as _;
use workspace::{ModalView, OpenOptions, OpenVisible, Workspace};

use crate::results_panel::ResultsPanel;
use crate::results_viewer::ResultsViewer;
use crate::run_query::{KustoSettings, RerunQuery, display};

actions!(
    kusto,
    [
        /// Lists the queries that have been run, to show the result of one again.
        ShowHistory,
    ]
);

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ShowHistory, window, cx| {
        show_history(workspace, window, cx)
    });
}

/// Where results and the run log are kept.
pub(crate) fn history_folder() -> PathBuf {
    paths::data_dir().join("kusto").join("history")
}

/// Deletes the oldest results beyond the limits when Zed starts, since a result is only ever
/// written while Zed runs, and nothing is open yet.
pub(crate) fn prune_at_startup(cx: &mut App) {
    let fs = <dyn Fs>::global(cx);
    let limits = KustoSettings::get_global(cx).history_limits;
    cx.background_spawn(async move { prune(fs.as_ref(), limits, &[]).await.log_err() })
        .detach();
}

/// Deletes the oldest results beyond the limits, except those in `keep`. Returns how many went.
pub(crate) async fn prune(fs: &dyn Fs, limits: HistoryLimits, keep: &[PathBuf]) -> Result<usize> {
    let folder = history_folder();
    let mut entries = match fs.read_dir(&folder).await {
        Ok(entries) => entries,
        Err(_) if !fs.is_dir(&folder).await => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    while let Some(path) = entries.next().await {
        let path = path?;
        if let Some(metadata) = fs.metadata(&path).await? {
            files.push(HistoryFile {
                path,
                size: metadata.len,
            });
        }
    }

    let doomed = files_to_prune(files, keep, limits);
    for path in &doomed {
        fs.remove_file(
            path,
            RemoveOptions {
                recursive: false,
                ignore_if_not_exists: true,
            },
        )
        .await?;
    }
    Ok(doomed.len())
}

/// The results this workspace has open, which pruning must leave alone.
pub(crate) fn open_results(workspace: &Workspace, cx: &App) -> Vec<PathBuf> {
    let project = workspace.project().read(cx);
    let tabs = workspace.items_of_type::<ResultsViewer>(cx);
    let panel = workspace
        .panel::<ResultsPanel>(cx)
        .and_then(|panel| panel.read(cx).shown_viewer().cloned());
    tabs.chain(panel)
        .filter_map(|viewer| project.absolute_path(&viewer.read(cx).project_path(cx), cx))
        .collect()
}

/// Deletes the oldest results beyond the limits after a run, leaving alone those that are open.
pub(crate) async fn prune_after_run(
    workspace: &WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    cx: &mut AsyncWindowContext,
) {
    let Some((limits, keep)) = workspace
        .update(cx, |workspace, cx| {
            (
                KustoSettings::get_global(cx).history_limits,
                open_results(workspace, cx),
            )
        })
        .log_err()
    else {
        return;
    };
    prune(fs.as_ref(), limits, &keep).await.log_err();
}

fn show_history(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let fs = workspace.app_state().fs.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let rows = load_rows(fs.as_ref()).await;
        workspace
            .update_in(cx, |workspace, window, cx| {
                let weak_workspace = cx.weak_entity();
                workspace.toggle_modal(window, cx, move |window, cx| {
                    HistorySelector::new(rows, fs, weak_workspace, window, cx)
                });
            })
            .log_err();
    })
    .detach();
}

/// A run that can be listed, and whether its result is still on disk.
#[derive(Clone, Debug)]
struct Row {
    entry: HistoryEntry,
    available: bool,
}

async fn load_rows(fs: &dyn Fs) -> Vec<Row> {
    let log = fs
        .load(&history_folder().join(RUN_LOG_FILE))
        .await
        .unwrap_or_default();
    let mut rows = Vec::new();
    for entry in history_entries(&log) {
        let available = match &entry.outcome {
            HistoryOutcome::Finished { path, .. } => fs.is_file(path).await,
            HistoryOutcome::Failed { .. } => true,
            HistoryOutcome::NoResult => continue,
        };
        rows.push(Row { entry, available });
    }
    rows
}

/// What tells a query apart in the list: the comments it starts with, which people write for that
/// purpose, then its first line of code. Directives that say where it runs are not comments to
/// read.
fn title(query: &str) -> String {
    let mut comments = Vec::new();
    let mut code = None;
    for line in query.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match line.strip_prefix("//") {
            Some(comment) => {
                let comment = comment.trim();
                if !comment.is_empty() && !comment.starts_with(':') {
                    comments.push(comment);
                }
            }
            None => {
                code = Some(line);
                break;
            }
        }
    }
    match (comments.is_empty(), code) {
        (true, Some(code)) => code.to_string(),
        (true, None) => "(no query text)".to_string(),
        (false, Some(code)) => format!("{} — {code}", comments.join(" · ")),
        (false, None) => comments.join(" · "),
    }
}

/// What a row says about its run: when, how it went and where.
fn describe<Zone: TimeZone>(row: &Row, now: DateTime<Zone>) -> String
where
    Zone::Offset: std::fmt::Display,
{
    let entry = &row.entry;
    let started = DateTime::parse_from_rfc3339(&entry.at)
        .ok()
        .map(|at| at.with_timezone(&now.timezone()));
    let when = match started {
        Some(at) if at.date_naive() == now.date_naive() => at.format("%H:%M:%S").to_string(),
        Some(at) => at.format("%b %-d, %H:%M").to_string(),
        None => String::new(),
    };
    let place = format!("{} / {}", entry.cluster, entry.database);
    let outcome = match (&entry.outcome, row.available) {
        (HistoryOutcome::Finished { .. }, false) => "result deleted".to_string(),
        (
            HistoryOutcome::Finished {
                duration_ms, rows, ..
            },
            true,
        ) => format!(
            "{} {}, {}",
            grouped(*rows),
            if *rows == 1 { "row" } else { "rows" },
            if *duration_ms < 1000 {
                format!("{duration_ms} ms")
            } else {
                format!("{:.1} s", *duration_ms as f64 / 1000.0)
            }
        ),
        (HistoryOutcome::Failed { message }, _) => {
            format!(
                "failed: {}",
                message.lines().next().unwrap_or_default().trim()
            )
        }
        (HistoryOutcome::NoResult, _) => String::new(),
    };
    [when, outcome, place]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The action that runs an entry again, as it was run.
fn rerun_action(entry: &HistoryEntry) -> RerunQuery {
    RerunQuery {
        query: entry.query.clone(),
        cluster: entry.cluster.clone(),
        database: entry.database.clone(),
        parameters: entry.parameters.clone(),
    }
}

/// A count with a comma between thousands, as the lenses show it.
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

struct HistorySelector {
    picker: Entity<Picker<HistoryDelegate>>,
}

impl HistorySelector {
    fn new(
        rows: Vec<Row>,
        fs: Arc<dyn Fs>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = HistoryDelegate {
            selector: cx.entity().downgrade(),
            workspace,
            fs,
            matches: (0..rows.len()).collect(),
            rows,
            query: String::new(),
            selected_index: 0,
        };
        let picker =
            cx.new(|cx| Picker::uniform_list(delegate, window, cx).initial_width(rems(40.)));
        Self { picker }
    }
}

impl Render for HistorySelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

impl Focusable for HistorySelector {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for HistorySelector {}
impl ModalView for HistorySelector {}

struct HistoryDelegate {
    selector: WeakEntity<HistorySelector>,
    workspace: WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    rows: Vec<Row>,
    /// Indexes into `rows` of the rows that match what was typed.
    matches: Vec<usize>,
    query: String,
    selected_index: usize,
}

impl HistoryDelegate {
    fn matching(rows: &[Row], query: &str) -> Vec<usize> {
        let query = query.to_lowercase();
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                let entry = &row.entry;
                let message = match &entry.outcome {
                    HistoryOutcome::Failed { message } => message.as_str(),
                    _ => "",
                };
                [&entry.query, &entry.cluster, &entry.database, message]
                    .into_iter()
                    .any(|text| text.to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_row(&self) -> Option<&Row> {
        self.rows.get(*self.matches.get(self.selected_index)?)
    }

    /// Closes the picker and runs the query of a row again.
    fn rerun(&mut self, row_index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(row) = self.rows.get(row_index) else {
            return;
        };
        let action = rerun_action(&row.entry);
        self.dismissed(window, cx);
        window.dispatch_action(action.boxed_clone(), cx);
    }

    fn delete(&mut self, row_index: usize, cx: &mut Context<Picker<Self>>) {
        let Some(row) = self.rows.get_mut(row_index) else {
            return;
        };
        let HistoryOutcome::Finished { path, .. } = &row.entry.outcome else {
            return;
        };
        let path = path.clone();
        row.available = false;
        let fs = self.fs.clone();
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            let removed = fs
                .remove_file(
                    &path,
                    RemoveOptions {
                        recursive: false,
                        ignore_if_not_exists: true,
                    },
                )
                .await;
            if let Err(error) = removed {
                workspace
                    .update(cx, |workspace, cx| workspace.show_error(error, cx))
                    .log_err();
            }
        })
        .detach();
        cx.notify();
    }
}

impl PickerDelegate for HistoryDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "query history"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search the queries that were run…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.matches = Self::matching(&self.rows, &query);
        self.query = query;
        self.selected_index = 0;
        Task::ready(())
    }

    fn confirm(&mut self, in_a_tab: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            show_row(&workspace, row, in_a_tab, cx).await
        })
        .detach();
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selector
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let row_index = *self.matches.get(ix)?;
        let row = self.rows.get(row_index)?;
        let mut item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(
                v_flex()
                    .child(Label::new(title(&row.entry.query)).truncate())
                    .child(
                        Label::new(describe(row, Local::now()))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            );
        let can_delete =
            matches!(row.entry.outcome, HistoryOutcome::Finished { .. }) && row.available;
        item = item.end_slot_on_hover(
            h_flex()
                .gap_0p5()
                .child(
                    IconButton::new(("rerun-query", ix), IconName::PlayFilled)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Run this query again"))
                        .on_click(cx.listener(move |picker, _, window, cx| {
                            cx.stop_propagation();
                            picker.delegate.rerun(row_index, window, cx);
                        })),
                )
                .when(can_delete, |buttons| {
                    buttons.child(
                        IconButton::new(("delete-result", ix), IconName::Trash)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Delete this result"))
                            .on_click(cx.listener(move |picker, _, _, cx| {
                                cx.stop_propagation();
                                picker.delegate.delete(row_index, cx);
                            })),
                    )
                }),
        );
        Some(item)
    }
}

/// Shows what a row stands for: its result in the Results panel (or a tab of its own when asked),
/// or why the run failed.
async fn show_row(
    workspace: &WeakEntity<Workspace>,
    row: Row,
    in_a_tab: bool,
    cx: &mut AsyncWindowContext,
) {
    let outcome = match row.entry.outcome {
        HistoryOutcome::Finished { path, .. } if row.available => Ok(path),
        HistoryOutcome::Finished { .. } => Err(deleted()),
        HistoryOutcome::Failed { message } => Err(anyhow!(message)),
        HistoryOutcome::NoResult => return,
    };
    match outcome {
        Ok(path) if in_a_tab => {
            workspace
                .update_in(cx, |workspace, window, cx| {
                    open_in_a_tab(workspace, path, window, cx)
                })
                .log_err();
        }
        outcome => display(workspace, outcome, cx).await,
    }
}

pub(crate) fn deleted() -> anyhow::Error {
    anyhow!("That result was deleted to keep the history within its limits.")
}

fn open_in_a_tab(
    workspace: &mut Workspace,
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace
        .open_abs_path(
            path,
            OpenOptions {
                visible: Some(OpenVisible::None),
                ..Default::default()
            },
            window,
            cx,
        )
        .detach_and_prompt_err("Could not open the query result", window, cx, |_, _, _| {
            None
        });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::FixedOffset;

    use super::*;

    fn row(outcome: HistoryOutcome, available: bool) -> Row {
        Row {
            entry: HistoryEntry {
                cid: "id".into(),
                query: "// a note\n\nStormEvents\n| take 1".into(),
                cluster: "help.kusto.windows.net".into(),
                database: "Samples".into(),
                at: "2026-10-01T14:30:05.000Z".into(),
                parameters: BTreeMap::new(),
                outcome,
            },
            available,
        }
    }

    fn finished(rows: usize, duration_ms: u64) -> HistoryOutcome {
        HistoryOutcome::Finished {
            duration_ms,
            rows,
            path: PathBuf::from("/h/a.ktt"),
        }
    }

    fn at(offset_hours: i32, day: u32, hour: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(offset_hours * 3600)
            .expect("an offset")
            .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
            .single()
            .expect("a time")
    }

    #[test]
    fn a_title_is_the_leading_comments_and_then_the_first_line_of_code() {
        assert_eq!(title("StormEvents\n| take 1"), "StormEvents");
        assert_eq!(
            title("// incident 123\n//   second try\n\n  StormEvents\n| take 1"),
            "incident 123 · second try — StormEvents"
        );
        assert_eq!(
            title("// :setDefaultDb(\"db\")\n// for the report\nT | take 1"),
            "for the report — T | take 1"
        );
        assert_eq!(
            title("// :setDefaultCluster(\"https://a\")\nT"),
            "T",
            "a directive says nothing about the query"
        );
        assert_eq!(
            title("T\n// a later comment"),
            "T",
            "only comments at the start count"
        );
        assert_eq!(title("// only a note"), "only a note");
        assert_eq!(title("//\n"), "(no query text)");
        assert_eq!(title(""), "(no query text)");
    }

    #[test]
    fn a_run_of_today_shows_the_time_and_an_older_one_the_date_in_local_time() {
        let finished = row(finished(1240, 1840), true);
        assert_eq!(
            describe(&finished, at(2, 1, 20)),
            "16:30:05 · 1,240 rows, 1.8 s · help.kusto.windows.net / Samples"
        );
        assert_eq!(
            describe(&finished, at(2, 3, 9)),
            "Oct 1, 16:30 · 1,240 rows, 1.8 s · help.kusto.windows.net / Samples"
        );
    }

    #[test]
    fn rerunning_a_row_names_its_query_place_and_parameter_values() {
        let mut row = row(finished(3, 10), true);
        row.entry.parameters.insert("raid".into(), "abc".into());
        let action = rerun_action(&row.entry);
        assert_eq!(action.query, row.entry.query);
        assert_eq!(action.cluster, "help.kusto.windows.net");
        assert_eq!(action.database, "Samples");
        assert_eq!(
            action.parameters,
            BTreeMap::from([("raid".to_string(), "abc".to_string())])
        );
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    #[test]
    fn rows_and_short_runs_are_worded_to_fit() {
        assert!(describe(&row(finished(1, 40), true), at(0, 1, 20)).contains("1 row, 40 ms"));
    }

    #[test]
    fn a_failed_run_gives_the_first_line_of_its_message() {
        let failed = row(
            HistoryOutcome::Failed {
                message: "Semantic error: SEM0100\nmore detail".into(),
            },
            true,
        );
        assert_eq!(
            describe(&failed, at(0, 1, 20)),
            "14:30:05 · failed: Semantic error: SEM0100 · help.kusto.windows.net / Samples"
        );
    }

    #[test]
    fn a_deleted_result_says_so() {
        assert!(describe(&row(finished(3, 10), false), at(0, 1, 20)).contains("result deleted"));
    }

    #[test]
    fn typing_matches_the_query_the_place_and_the_error() {
        let rows = vec![
            row(finished(1, 10), true),
            Row {
                entry: HistoryEntry {
                    query: "print 1".into(),
                    cluster: "other.kusto.windows.net".into(),
                    ..row(
                        HistoryOutcome::Failed {
                            message: "Boom".into(),
                        },
                        true,
                    )
                    .entry
                },
                available: true,
            },
        ];
        assert_eq!(HistoryDelegate::matching(&rows, ""), [0, 1]);
        assert_eq!(HistoryDelegate::matching(&rows, "STORM"), [0]);
        assert_eq!(HistoryDelegate::matching(&rows, "other"), [1]);
        assert_eq!(HistoryDelegate::matching(&rows, "boom"), [1]);
        assert!(HistoryDelegate::matching(&rows, "nothing").is_empty());
    }

    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;

    use crate::run_query::tests::{ANSWER, run, setup};

    fn fs_of(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> Arc<dyn Fs> {
        workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone())
    }

    fn selector(
        workspace: &Entity<Workspace>,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<HistorySelector> {
        workspace
            .read_with(cx, |workspace, cx| {
                workspace.active_modal::<HistorySelector>(cx)
            })
            .expect("the history is open")
    }

    fn open_history(
        workspace: &Entity<Workspace>,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<HistorySelector> {
        workspace.update_in(cx, |workspace, window, cx| {
            show_history(workspace, window, cx)
        });
        cx.run_until_parked();
        selector(workspace, cx)
    }

    async fn history_files(fs: &dyn Fs) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(mut entries) = fs.read_dir(&history_folder()).await {
            while let Some(path) = entries.next().await {
                if let Some(name) = path.ok().and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }) && name.ends_with(".ktt")
                {
                    names.push(name);
                }
            }
        }
        names.sort();
        names
    }

    #[gpui::test]
    async fn the_history_lists_runs_newest_first_and_shows_the_one_chosen(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        run(&workspace, cx);
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        panel.update(cx, |panel, cx| panel.show_error("something else", cx));

        let selector = open_history(&workspace, cx);
        let picker = selector.read_with(cx, |selector, _| selector.picker.clone());
        let rows = picker.read_with(cx, |picker, _| picker.delegate.rows.clone());
        let width =
            cx.update(|window, cx| picker.read(cx).results_width(window) / window.rem_size());
        assert!(
            (width - 40.).abs() < 0.01,
            "the picker takes the width it asked for, not its wrapper's: {width} rem"
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].entry.at >= rows[1].entry.at, "newest first");
        assert!(rows.iter().all(|row| row.available));
        assert_eq!(title(&rows[0].entry.query), "StormEvents");

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx)
        });
        cx.run_until_parked();

        let shown = panel.read_with(cx, |panel, cx| {
            panel
                .shown_viewer()
                .map(|viewer| viewer.read(cx).result(cx).total_rows())
        });
        assert_eq!(shown, Some(2), "the chosen result replaced what was shown");
        assert!(
            workspace.read_with(cx, |workspace, cx| workspace
                .active_modal::<HistorySelector>(cx)
                .is_none()),
            "the picker closes"
        );
    }

    #[gpui::test]
    async fn a_result_that_was_deleted_is_marked_and_says_so_when_chosen(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        let fs = fs_of(&workspace, cx);
        for name in history_files(fs.as_ref()).await {
            fs.remove_file(&history_folder().join(name), RemoveOptions::default())
                .await
                .expect("the result is removed");
        }

        let selector = open_history(&workspace, cx);
        let picker = selector.read_with(cx, |selector, _| selector.picker.clone());
        let rows = picker.read_with(cx, |picker, _| picker.delegate.rows.clone());
        assert!(!rows[0].available);
        assert!(describe(&rows[0], Local::now()).contains("result deleted"));

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx)
        });
        cx.run_until_parked();
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        let error = panel.read_with(cx, |panel, _| panel.shown_error().map(str::to_string));
        assert!(error.is_some_and(|error| error.contains("was deleted")));
    }

    #[gpui::test]
    async fn the_trash_button_deletes_the_file_and_marks_the_row(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        let fs = fs_of(&workspace, cx);
        assert_eq!(history_files(fs.as_ref()).await.len(), 1);

        let selector = open_history(&workspace, cx);
        let picker = selector.read_with(cx, |selector, _| selector.picker.clone());
        picker.update(cx, |picker, cx| picker.delegate.delete(0, cx));
        cx.run_until_parked();

        assert!(history_files(fs.as_ref()).await.is_empty());
        assert!(!picker.read_with(cx, |picker, _| picker.delegate.rows[0].available));
    }

    #[gpui::test]
    async fn a_run_past_the_limit_deletes_the_oldest_results_but_not_the_one_it_shows(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{ "kusto": { "cluster": "https://help.kusto.windows.net", "database": "Samples", "history_max_results": 2 } }"#,
                        cx,
                    )
                    .expect("the user settings parse");
            });
        });
        let fs = fs_of(&workspace, cx);
        for _ in 0..4 {
            run(&workspace, cx);
            // Result names hold the second the run began, so runs in one second need telling apart.
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(2));
        }

        let files = history_files(fs.as_ref()).await;
        // The runs happen in one second of real time, so which result sorts newest is down to
        // the random part of the name. The open result is kept whichever it is, so there may be
        // one more than the limit.
        assert!((2..=3).contains(&files.len()), "{files:?}");
        let shown = workspace.read_with(cx, |workspace, cx| open_results(workspace, cx));
        assert_eq!(shown.len(), 1, "the panel shows a result");
        assert!(
            files.iter().any(|name| shown[0].ends_with(name)),
            "{shown:?} is not among {files:?}"
        );
    }

    #[gpui::test]
    async fn pruning_leaves_files_that_are_not_runs_and_the_ones_to_keep(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let id = "da6a8e58-6566-4f58-bc02-34659abf407a";
        let folder = history_folder();
        fs.create_dir(&folder).await.expect("the folder is made");
        for stamp in ["20261001-100000", "20261001-110000", "20261001-120000"] {
            fs.insert_file(folder.join(format!("{stamp}-{id}.ktt")), b"result".to_vec())
                .await;
        }
        fs.insert_file(folder.join("mine.ktt"), b"saved by hand".to_vec())
            .await;
        fs.insert_file(folder.join(RUN_LOG_FILE), b"{}".to_vec())
            .await;
        let oldest = folder.join(format!("20261001-100000-{id}.ktt"));
        let limits = HistoryLimits {
            max_results: 1,
            max_bytes: 0,
        };

        let removed = prune(fs.as_ref(), limits, std::slice::from_ref(&oldest))
            .await
            .expect("pruning works");

        assert_eq!(removed, 1);
        let left = history_files(fs.as_ref()).await;
        assert_eq!(
            left,
            [
                "20261001-100000-da6a8e58-6566-4f58-bc02-34659abf407a.ktt".to_string(),
                "20261001-120000-da6a8e58-6566-4f58-bc02-34659abf407a.ktt".to_string(),
                "mine.ktt".to_string(),
            ]
        );
        assert!(fs.is_file(&folder.join(RUN_LOG_FILE)).await);
    }

    #[gpui::test]
    async fn pruning_a_folder_that_does_not_exist_is_nothing(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        let limits = HistoryLimits {
            max_results: 1,
            max_bytes: 1,
        };
        assert_eq!(
            prune(fs.as_ref(), limits, &[])
                .await
                .expect("nothing to do"),
            0
        );
        let _ = json!({});
    }
}
