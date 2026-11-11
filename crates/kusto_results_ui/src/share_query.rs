//! Sharing a query: a dialog that asks which of a link to the query, the query itself and the rows
//! of its result to copy, as the Azure Data Explorer web UI does. The rows are a Markdown table or,
//! as in the web UI, a rich-text table that plain text pastes as tab-separated rows.
//!
//! The query and where it ran come from a result file when there is one, which also holds the
//! rows, and otherwise from the query around the cursor, written as a copy writes it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor::{Editor, EditorEvent};
use fs::Fs;
use gpui::{
    Action, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, SharedString, Subscription, Task, WeakEntity, Window, rems,
};
use gpui_util::ResultExt as _;
use kusto_client::{
    ResultsFormat, ResultsPreview, Shared, results_preview, share_html, share_text,
    web_explorer_link,
};
use kusto_results::{ResultSet, Table};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::prelude::*;
use ui::{
    Checkbox, ToggleButtonGroup, ToggleButtonGroupSize, ToggleButtonGroupStyle, ToggleButtonSimple,
    ToggleState,
};
use workspace::notifications::NotificationId;
use workspace::{ModalView, Toast, Workspace};

use crate::results_panel::ResultsPanel;
use crate::results_viewer::ResultsViewer;
use crate::run_query::{PortableQueryAtCursor, load_result_file, portable_query_at_cursor};

/// Copies a link that opens a query in Azure Data Explorer, the query, and the rows of its
/// result, as chosen in a dialog. The query is the one of the result file at `path`, or else the
/// query around the cursor, which has no rows to show.
#[derive(Clone, PartialEq, Debug, Default, Deserialize, JsonSchema, Action)]
#[action(namespace = kusto)]
#[serde(deny_unknown_fields)]
pub struct ShareQuery {
    /// The result file to share the query and rows of. When missing, the query around the cursor
    /// of the active editor, or else the result of the active tab or of the Results panel.
    #[serde(default)]
    pub path: Option<String>,
}

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, action: &ShareQuery, window, cx| {
        let editor = workspace.active_item_as::<Editor>(cx);
        share(workspace, editor, action, window, cx)
    });
}

/// Shares for a thread's editor, which is in no pane.
pub(crate) fn share_thread_query(
    workspace: &Entity<Workspace>,
    editor: &Entity<Editor>,
    action: &ShareQuery,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.update(cx, |workspace, cx| {
        share(workspace, Some(editor.clone()), action, window, cx)
    });
}

/// What was chosen, kept between uses so that the dialog starts as it was left. With `remember`
/// the dialog is not shown at all, and a share copies what was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ShareChoice {
    link: bool,
    query: bool,
    results_preview: bool,
    format: ResultsFormat,
    /// How many rows to take from the top of the result; all of them when `None`.
    top_rows: Option<usize>,
    remember: bool,
}

impl Default for ShareChoice {
    fn default() -> Self {
        Self {
            link: true,
            query: true,
            results_preview: true,
            format: ResultsFormat::default(),
            top_rows: None,
            remember: false,
        }
    }
}

/// How many rows "Top" starts at.
const DEFAULT_TOP_ROWS: usize = 100;

/// How many rows the dialog draws of the table. The table that is copied is not cut to this.
const DISPLAYED_ROWS: usize = 20;

const CHOICE_FILE: &str = "share.json";

fn choice_path() -> PathBuf {
    paths::data_dir().join("kusto").join(CHOICE_FILE)
}

async fn load_choice(fs: &dyn Fs) -> ShareChoice {
    let path = choice_path();
    if !fs.is_file(&path).await {
        return ShareChoice::default();
    }
    let loaded = async {
        let text = fs.load(&path).await?;
        Ok::<_, anyhow::Error>(serde_json::from_str(&text)?)
    }
    .await;
    loaded.log_err().unwrap_or_default()
}

async fn save_choice(fs: &dyn Fs, choice: ShareChoice) -> Result<()> {
    let path = choice_path();
    if let Some(folder) = path.parent() {
        fs.create_dir(folder).await?;
    }
    fs.write(&path, serde_json::to_string(&choice)?.as_bytes())
        .await
        .context("could not keep what was chosen to share")
}

/// What there is to share.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    query: String,
    /// The link, or why there is none.
    link: Result<String, SharedString>,
    /// The first table of the result; a query that was not run has none.
    table: Option<Arc<Table>>,
}

impl Draft {
    async fn new(
        query: String,
        cluster: Option<&str>,
        database: Option<&str>,
        table: Option<Table>,
    ) -> Self {
        fn given(text: Option<&str>) -> Option<&str> {
            text.map(str::trim).filter(|text| !text.is_empty())
        }
        let link = match given(cluster).zip(given(database)) {
            Some((cluster, database)) => web_explorer_link(cluster, database, &query)
                .await
                .map_err(|error| SharedString::from(format!("{error:#}"))),
            None => Err("The query does not say which cluster and database it runs on.".into()),
        };
        Self {
            query,
            link,
            table: table
                .filter(|table| !table.columns.is_empty())
                .map(Arc::new),
        }
    }

    async fn of_result(result: &ResultSet) -> Result<Self> {
        let query = result
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .context("This result does not keep its query, so there is nothing to share.")?
            .to_string();
        Ok(Self::new(
            query,
            result.cluster.as_deref(),
            result.database.as_deref(),
            result.tables.first().cloned(),
        )
        .await)
    }

    /// The rows, cut to `limit` rows when there is one, written as `choice` says.
    fn results(&self, choice: ShareChoice, limit: Option<usize>) -> Option<ResultsPreview> {
        if !choice.results_preview {
            return None;
        }
        results_preview(self.table.as_deref()?, limit, choice.format)
    }

    fn shared<'a>(
        &'a self,
        choice: ShareChoice,
        results: Option<&'a ResultsPreview>,
    ) -> Shared<'a> {
        Shared {
            link: self.link.as_deref().ok().filter(|_| choice.link),
            query: choice.query.then_some(self.query.as_str()),
            results,
        }
    }

    /// The text of the parts that were chosen and that there are.
    fn text(&self, choice: ShareChoice) -> String {
        let results = self.results(choice, choice.top_rows);
        share_text(self.shared(choice, results.as_ref()))
    }

    /// The rich text of the same, when the rows are a rich-text table.
    fn html(&self, choice: ShareChoice) -> Option<String> {
        let results = self.results(choice, choice.top_rows);
        share_html(self.shared(choice, results.as_ref()))
    }

    /// What the dialog shows of the text: the table is cut to what the dialog has room for, and
    /// its note is left to the dialog, which says how many rows will be copied.
    fn displayed_text(&self, choice: ShareChoice) -> String {
        let limit = choice
            .top_rows
            .map_or(DISPLAYED_ROWS, |rows| rows.min(DISPLAYED_ROWS));
        let results = self
            .results(choice, Some(limit))
            .map(|results| ResultsPreview {
                text: results.text_without_note().to_string(),
                html: None,
                note: None,
            });
        share_text(self.shared(choice, results.as_ref()))
    }

    fn total_rows(&self) -> usize {
        self.table.as_ref().map_or(0, |table| table.rows.len())
    }

    /// How many rows `choice` copies.
    fn copied_rows(&self, choice: ShareChoice) -> usize {
        let total = self.total_rows();
        choice.top_rows.map_or(total, |rows| rows.min(total))
    }
}

enum Source {
    File(PathBuf),
    Cursor(Task<Result<PortableQueryAtCursor>>),
}

fn share(
    workspace: &mut Workspace,
    editor: Option<Entity<Editor>>,
    action: &ShareQuery,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let source = if let Some(path) = &action.path {
        Some(Source::File(PathBuf::from(path)))
    } else if let Some(editor) = editor {
        match portable_query_at_cursor(workspace, &editor, cx) {
            Some(task) => Some(Source::Cursor(task)),
            None => {
                workspace.show_error(anyhow!("There is no query at the cursor."), cx);
                return;
            }
        }
    } else {
        workspace
            .active_item_as::<ResultsViewer>(cx)
            .or_else(|| {
                workspace
                    .panel::<ResultsPanel>(cx)
                    .and_then(|panel| panel.read(cx).shown_viewer().cloned())
            })
            .map(|viewer| Source::File(viewer.read(cx).path_on_disk(cx)))
    };
    let Some(source) = source else {
        workspace.show_error(anyhow!("Open a Kusto query or a result to share it."), cx);
        return;
    };
    let fs = workspace.app_state().fs.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let draft = async {
            match source {
                Source::File(path) => {
                    if !fs.is_file(&path).await {
                        return Err(crate::history::deleted());
                    }
                    let file = load_result_file(&workspace, path, cx).await?;
                    let result = file.read_with(cx, |file, _| file.result());
                    Draft::of_result(&result).await
                }
                Source::Cursor(task) => {
                    let portable = task.await?;
                    Ok(Draft::new(
                        portable.text,
                        portable.connection.cluster.as_deref(),
                        portable.connection.database.as_deref(),
                        None,
                    )
                    .await)
                }
            }
        }
        .await;
        let choice = load_choice(fs.as_ref()).await;
        workspace
            .update_in(cx, |workspace, window, cx| match draft {
                Ok(draft) => present(workspace, draft, choice, window, cx),
                Err(error) => workspace.show_error(error, cx),
            })
            .log_err();
    })
    .detach();
}

/// Asks what to share, or, when the person said to remember their choice, shares it.
fn present(
    workspace: &mut Workspace,
    draft: Draft,
    choice: ShareChoice,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if choice.remember && !draft.text(choice).is_empty() {
        copy(workspace, &draft, choice, cx);
    } else {
        ask(workspace, draft, choice, window, cx);
    }
}

fn ask(
    workspace: &mut Workspace,
    draft: Draft,
    choice: ShareChoice,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = workspace.weak_handle();
    workspace.toggle_modal(window, cx, move |window, cx| {
        ShareModal::new(draft, choice, workspace_handle, window, cx)
    });
}

fn copy(
    workspace: &mut Workspace,
    draft: &Draft,
    choice: ShareChoice,
    cx: &mut Context<Workspace>,
) {
    let text = draft.text(choice);
    match draft.html(choice) {
        Some(html) => cx.write_text_and_html_to_clipboard(text, html),
        None => cx.write_to_clipboard(ClipboardItem::new_string(text)),
    }
    let fs = workspace.app_state().fs.clone();
    cx.spawn(async move |_, _| save_choice(fs.as_ref(), choice).await.log_err())
        .detach();
    let mut toast = Toast::new(
        NotificationId::unique::<ShareQuery>(),
        "Copied the shared query",
    )
    .autohide();
    if choice.remember {
        // A remembered choice skips the dialog, so the way back to it is here.
        let workspace_handle = workspace.weak_handle();
        let draft = draft.clone();
        toast = toast.on_click("Change", move |window, cx| {
            workspace_handle
                .update(cx, |workspace, cx| {
                    ask(workspace, draft.clone(), choice, window, cx)
                })
                .log_err();
        });
    }
    workspace.show_toast(toast, cx);
}

pub(crate) struct ShareModal {
    draft: Draft,
    choice: ShareChoice,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    /// Where the number of rows to take is typed.
    rows_editor: Entity<Editor>,
    /// Whether what is typed in `rows_editor` is not a number of rows, while the rows are cut.
    rows_invalid: bool,
    _rows_subscription: Subscription,
}

impl EventEmitter<DismissEvent> for ShareModal {}
impl ModalView for ShareModal {}

impl Focusable for ShareModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ShareModal {
    fn new(
        draft: Draft,
        choice: ShareChoice,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rows = choice.top_rows.unwrap_or(DEFAULT_TOP_ROWS).to_string();
        let rows_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(rows, window, cx);
            editor.set_placeholder_text("Rows", window, cx);
            editor
        });
        let rows_subscription =
            cx.subscribe(&rows_editor, |this, editor, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    let text = editor.read(cx).text(cx);
                    this.typed_rows(&text, cx);
                }
            });
        Self {
            draft,
            choice,
            workspace,
            focus_handle: cx.focus_handle(),
            rows_editor,
            rows_invalid: false,
            _rows_subscription: rows_subscription,
        }
    }

    /// Takes the number of rows typed. Typing changes nothing while every row is taken.
    fn typed_rows(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.choice.top_rows.is_none() {
            return;
        }
        match text.trim().parse::<usize>() {
            Ok(rows) if rows > 0 => {
                self.choice.top_rows = Some(rows);
                self.rows_invalid = false;
            }
            _ => self.rows_invalid = true,
        }
        cx.notify();
    }

    fn take_all_rows(&mut self, cx: &mut Context<Self>) {
        self.choice.top_rows = None;
        self.rows_invalid = false;
        cx.notify();
    }

    fn take_top_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.rows_editor.read(cx).text(cx);
        let rows = match text.trim().parse::<usize>() {
            Ok(rows) if rows > 0 => rows,
            _ => {
                self.rows_editor.update(cx, |editor, cx| {
                    editor.set_text(DEFAULT_TOP_ROWS.to_string(), window, cx)
                });
                DEFAULT_TOP_ROWS
            }
        };
        self.choice.top_rows = Some(rows);
        self.rows_invalid = false;
        self.rows_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn can_copy(&self) -> bool {
        !self.rows_invalid && !self.draft.text(self.choice).is_empty()
    }

    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.draft.query
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_copy() {
            return;
        }
        let (draft, choice) = (self.draft.clone(), self.choice);
        self.workspace
            .update(cx, |workspace, cx| copy(workspace, &draft, choice, cx))
            .log_err();
        cx.emit(DismissEvent);
    }

    /// A checkbox for one part. A part there is none of, such as the rows of a query that was not
    /// run, is unchecked and cannot be checked, and the tooltip says why.
    fn part(
        &self,
        id: &'static str,
        label: &'static str,
        chosen: bool,
        unavailable: Option<SharedString>,
        toggle: fn(&mut ShareChoice),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let modal = cx.entity();
        let disabled = unavailable.is_some();
        let checkbox = Checkbox::new(
            id,
            if chosen && !disabled {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            },
        )
        .label(label)
        .disabled(disabled)
        .on_click(move |_, _, cx| {
            modal.update(cx, |modal, cx| {
                toggle(&mut modal.choice);
                cx.notify();
            })
        });
        match unavailable {
            Some(reason) => checkbox
                .tooltip(move |_, cx| ui::Tooltip::simple(reason.clone(), cx))
                .into_any_element(),
            None => checkbox.into_any_element(),
        }
    }
}

/// What the dialog says about the rows that will be copied.
fn rows_summary(total: usize, copied: usize, format: ResultsFormat) -> String {
    let rows = |count: usize| match count {
        1 => "1 row".to_string(),
        count => format!("{count} rows"),
    };
    let mut summary = if copied == total {
        format!("Copying all {}.", rows(total))
    } else {
        format!("Copying {copied} of {}.", rows(total))
    };
    if format == ResultsFormat::RichText {
        summary.push_str(" Rich text pastes as a table, and as tab-separated rows in plain text.");
    }
    if copied > DISPLAYED_ROWS {
        summary.push_str(&format!(" The preview shows the first {DISPLAYED_ROWS}."));
    }
    summary
}

impl ShareModal {
    /// How the rows are written and how many are taken, under the checkbox of the rows.
    fn rows_options(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let choice = self.choice;
        let colors = cx.theme().colors().clone();
        let format = ToggleButtonGroup::single_row(
            "share-format",
            [
                ToggleButtonSimple::new(
                    "Markdown",
                    cx.listener(|this, _, _, cx| {
                        this.choice.format = ResultsFormat::Markdown;
                        cx.notify();
                    }),
                ),
                ToggleButtonSimple::new(
                    "Rich text",
                    cx.listener(|this, _, _, cx| {
                        this.choice.format = ResultsFormat::RichText;
                        cx.notify();
                    }),
                ),
            ],
        )
        .selected_index(match choice.format {
            ResultsFormat::Markdown => 0,
            ResultsFormat::RichText => 1,
        })
        .style(ToggleButtonGroupStyle::Outlined)
        .size(ToggleButtonGroupSize::Medium)
        .auto_width();
        let rows = ToggleButtonGroup::single_row(
            "share-rows",
            [
                ToggleButtonSimple::new(
                    "All rows",
                    cx.listener(|this, _, _, cx| this.take_all_rows(cx)),
                ),
                ToggleButtonSimple::new(
                    "Top",
                    cx.listener(|this, _, window, cx| this.take_top_rows(window, cx)),
                ),
            ],
        )
        .selected_index(usize::from(choice.top_rows.is_some()))
        .style(ToggleButtonGroupStyle::Outlined)
        .size(ToggleButtonGroupSize::Medium)
        .auto_width();
        let border = if self.rows_invalid {
            cx.theme().status().error_border
        } else if self
            .rows_editor
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            colors.border_focused
        } else {
            colors.border
        };
        let summary: SharedString = if self.rows_invalid {
            "Type a number of rows above zero.".into()
        } else {
            rows_summary(
                self.draft.total_rows(),
                self.draft.copied_rows(choice),
                choice.format,
            )
            .into()
        };
        v_flex()
            .pl_6()
            .gap_1p5()
            .child(format)
            .child(
                h_flex()
                    .gap_2()
                    .child(rows)
                    .when(choice.top_rows.is_some(), |row| {
                        row.child(
                            div()
                                .w_20()
                                .px_2()
                                .py_0p5()
                                .border_1()
                                .border_color(border)
                                .rounded_sm()
                                .child(self.rows_editor.clone()),
                        )
                    }),
            )
            .child(
                Label::new(summary)
                    .size(LabelSize::Small)
                    .color(if self.rows_invalid {
                        Color::Error
                    } else {
                        Color::Muted
                    }),
            )
            .into_any_element()
    }
}

impl Render for ShareModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.draft.displayed_text(self.choice);
        let can_copy = self.can_copy();
        let choice = self.choice;
        let has_rows = self.draft.table.is_some() && choice.results_preview;
        let rows_options = has_rows.then(|| self.rows_options(window, cx));
        let link = self.part(
            "share-link",
            "Link",
            choice.link,
            self.draft.link.as_ref().err().cloned(),
            |choice| choice.link = !choice.link,
            cx,
        );
        let query = self.part(
            "share-query",
            "Query code",
            choice.query,
            None,
            |choice| choice.query = !choice.query,
            cx,
        );
        let results = self.part(
            "share-results-preview",
            "Results preview",
            choice.results_preview,
            self.draft
                .table
                .is_none()
                .then(|| "This query has no result to show rows of.".into()),
            |choice| choice.results_preview = !choice.results_preview,
            cx,
        );
        let remember = self.part(
            "share-remember",
            "Remember my choice",
            choice.remember,
            None,
            |choice| choice.remember = !choice.remember,
            cx,
        );
        let colors = cx.theme().colors().clone();
        let modal = cx.entity();
        let copy_modal = modal.clone();
        v_flex()
            .key_context("ShareQueryModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w(rems(52.))
            .p_4()
            .gap_3()
            .child(Headline::new("Share").size(HeadlineSize::Small))
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_none()
                            .w_64()
                            .gap_2()
                            .child(link)
                            .child(query)
                            .child(results)
                            .children(rows_options),
                    )
                    .child(
                        div()
                            .id("share-preview")
                            .debug_selector(|| "share-preview".to_string())
                            .flex_1()
                            .min_w_0()
                            .h_48()
                            .p_2()
                            .overflow_y_scroll()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border_variant)
                            .bg(colors.editor_background)
                            .font_buffer(cx)
                            .text_ui_sm(cx)
                            .when(text.is_empty(), |preview| {
                                preview.child(
                                    Label::new("Choose something to copy.")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .children(text.lines().map(|line| {
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(if line.is_empty() {
                                        " ".to_string()
                                    } else {
                                        line.to_string()
                                    })
                            })),
                    ),
            )
            .child(
                h_flex().justify_between().child(remember).child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("share-copy", "Copy")
                                .style(ButtonStyle::Filled)
                                .disabled(!can_copy)
                                .on_click(move |_, window, cx| {
                                    copy_modal.update(cx, |modal, cx| {
                                        modal.confirm(&menu::Confirm, window, cx)
                                    })
                                }),
                        )
                        .child(Button::new("share-cancel", "Cancel").on_click(
                            move |_, window, cx| {
                                modal
                                    .update(cx, |modal, cx| modal.cancel(&menu::Cancel, window, cx))
                            },
                        )),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use futures::executor::block_on;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
    use workspace::{OpenOptions, OpenVisible};

    use super::*;
    use crate::run_query::tests::{ANSWER, QUERIES, setup, setup_in};

    /// The query at the cursor of `queries.kql`, as a copy writes it when no language server
    /// qualified its names.
    const CURSOR_QUERY: &str = "// :setDefaultCluster('https://help.kusto.windows.net')\n// :setDefaultDb('Samples')\nStormEvents\n| take 1";

    const TWELVE_ROWS: &str = r#"{
      "query": "StormEvents\n| where State == 'TEXAS'",
      "cluster": "help.kusto.windows.net",
      "database": "Samples",
      "tables": [{"name": "PrimaryResult", "columns": [{"name": "n", "type": "long"}],
                  "rows": [[0],[1],[2],[3],[4],[5],[6],[7],[8],[9],[10],[11]]}]
    }"#;
    const TWELVE_ROWS_QUERY: &str = "StormEvents\n| where State == 'TEXAS'";
    const ALL_TWELVE: &str = "| n |\n| --- |\n| 0 |\n| 1 |\n| 2 |\n| 3 |\n| 4 |\n| 5 |\n| 6 |\n| 7 |\n| 8 |\n| 9 |\n| 10 |\n| 11 |";
    const TOP_THREE: &str = "| n |\n| --- |\n| 0 |\n| 1 |\n| 2 |\n\n(3 of 12 rows)";

    const WITHOUT_CLUSTER: &str = r#"{
      "query": "StormEvents | take 1",
      "tables": [{"name": "PrimaryResult", "columns": [{"name": "n", "type": "long"}], "rows": [[1]]}]
    }"#;

    const WITHOUT_QUERY: &str = r#"{
      "tables": [{"name": "PrimaryResult", "columns": [{"name": "n", "type": "long"}], "rows": [[1]]}]
    }"#;

    fn link_to(query: &str) -> String {
        block_on(web_explorer_link(
            "https://help.kusto.windows.net",
            "Samples",
            query,
        ))
        .expect("a link")
    }

    fn share_with(
        workspace: &Entity<Workspace>,
        editor: Option<&Entity<Editor>>,
        path: Option<&str>,
        cx: &mut VisualTestContext,
    ) {
        let action = ShareQuery {
            path: path.map(str::to_string),
        };
        let editor = editor.cloned();
        workspace.update_in(cx, |workspace, window, cx| {
            share(workspace, editor, &action, window, cx)
        });
        cx.run_until_parked();
    }

    fn modal(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<ShareModal>> {
        workspace.read_with(cx, |workspace, cx| workspace.active_modal::<ShareModal>(cx))
    }

    fn change(modal: &Entity<ShareModal>, edit: fn(&mut ShareChoice), cx: &mut VisualTestContext) {
        modal.update(cx, |modal, _| edit(&mut modal.choice));
    }

    fn copy_from(modal: &Entity<ShareModal>, cx: &mut VisualTestContext) {
        modal.update_in(cx, |modal, window, cx| {
            modal.confirm(&menu::Confirm, window, cx)
        });
        cx.run_until_parked();
    }

    fn clipboard_text(cx: &mut VisualTestContext) -> Option<String> {
        cx.read_from_clipboard().and_then(|item| item.text())
    }

    fn error_shown(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<anyhow::Error>())
        })
    }

    async fn kept_choice(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> serde_json::Value {
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let text = fs.load(&choice_path()).await.expect("the choice is kept");
        serde_json::from_str(&text).expect("the choice is JSON")
    }

    #[gpui::test]
    async fn the_query_at_the_cursor_is_shared_with_a_link_to_it(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;

        share_with(&workspace, Some(&editor), None, cx);

        let modal = modal(&workspace, cx).expect("the dialog asks what to share");
        assert_eq!(
            clipboard_text(cx),
            None,
            "nothing is copied before the choice"
        );
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.draft.table, None, "the query was not run");
            assert_eq!(
                modal.draft.text(modal.choice),
                format!("{}\n\n{CURSOR_QUERY}", link_to(CURSOR_QUERY))
            );
        });

        copy_from(&modal, cx);
        assert_eq!(
            clipboard_text(cx),
            Some(format!("{}\n\n{CURSOR_QUERY}", link_to(CURSOR_QUERY)))
        );
        assert!(self::modal(&workspace, cx).is_none(), "the dialog closes");
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<ShareQuery>())
        }));
    }

    #[gpui::test]
    async fn the_link_opens_the_query_on_the_cluster_and_database_it_ran_on(
        cx: &mut TestAppContext,
    ) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        share_with(&workspace, Some(&editor), None, cx);
        let link = modal(&workspace, cx)
            .expect("the dialog")
            .read_with(cx, |modal, _| modal.draft.link.clone())
            .expect("a link");
        assert!(
            link.starts_with(
                "https://dataexplorer.azure.com/clusters/help/databases/Samples?query=H4sI"
            ),
            "{link}"
        );
    }

    #[gpui::test]
    async fn every_row_of_a_result_is_shared_with_its_query_and_link(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "shared.ktt": TWELVE_ROWS }),
        )
        .await;

        share_with(&workspace, None, Some("/root/shared.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog asks what to share");
        copy_from(&modal, cx);

        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "{}\n\n{TWELVE_ROWS_QUERY}\n\n{ALL_TWELVE}",
                link_to(TWELVE_ROWS_QUERY)
            ))
        );
    }

    #[gpui::test]
    async fn the_result_of_the_tab_that_is_shown_is_shared_when_no_path_is_given(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "shared.ktt": TWELVE_ROWS }),
        )
        .await;
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/root/shared.ktt").to_path_buf(),
                    OpenOptions {
                        visible: Some(OpenVisible::None),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            })
            .await
            .expect("the result opens");
        cx.run_until_parked();

        share_with(&workspace, None, None, cx);

        let modal = modal(&workspace, cx).expect("the dialog asks what to share");
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.draft.query, TWELVE_ROWS_QUERY);
            assert_eq!(
                modal.draft.text(modal.choice).split("\n\n").last(),
                Some(ALL_TWELVE)
            );
        });
    }

    async fn shared_result(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<ShareModal>,
        &mut VisualTestContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "shared.ktt": TWELVE_ROWS }),
        )
        .await;
        share_with(&workspace, None, Some("/root/shared.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog");
        (workspace, modal, cx)
    }

    #[gpui::test]
    async fn the_top_rows_are_shared_with_a_note_of_how_many_there_are(cx: &mut TestAppContext) {
        let (_workspace, modal, cx) = shared_result(cx).await;

        change(&modal, |choice| choice.top_rows = Some(3), cx);
        copy_from(&modal, cx);

        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "{}\n\n{TWELVE_ROWS_QUERY}\n\n{TOP_THREE}",
                link_to(TWELVE_ROWS_QUERY)
            ))
        );
    }

    #[gpui::test]
    async fn rich_text_is_a_table_over_tab_separated_rows(cx: &mut TestAppContext) {
        let (_workspace, modal, cx) = shared_result(cx).await;

        change(
            &modal,
            |choice| {
                choice.format = ResultsFormat::RichText;
                choice.top_rows = Some(2);
            },
            cx,
        );
        modal.read_with(cx, |modal, _| {
            let html = modal.draft.html(modal.choice).expect("rich text");
            assert!(
                html.starts_with("<p><a href=\"https://dataexplorer.azure.com/"),
                "{html}"
            );
            assert!(html.contains("<pre"), "{html}");
            assert_eq!(html.matches("<tr>").count(), 1 + 2, "{html}");
            assert!(html.ends_with("<p>(2 of 12 rows)</p>"), "{html}");
        });
        copy_from(&modal, cx);

        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "{}\n\n{TWELVE_ROWS_QUERY}\n\nn\n0\n1\n(2 of 12 rows)",
                link_to(TWELVE_ROWS_QUERY)
            ))
        );
    }

    #[gpui::test]
    async fn markdown_and_a_result_with_no_rows_part_have_no_rich_text(cx: &mut TestAppContext) {
        let (_workspace, modal, cx) = shared_result(cx).await;
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.draft.html(modal.choice), None)
        });

        change(
            &modal,
            |choice| {
                choice.format = ResultsFormat::RichText;
                choice.results_preview = false;
            },
            cx,
        );
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.draft.html(modal.choice), None)
        });
    }

    #[gpui::test]
    async fn the_dialog_shows_a_few_rows_and_copies_them_all(cx: &mut TestAppContext) {
        let rows: Vec<String> = (0..50).map(|row| format!("[{row}]")).collect();
        let many = format!(
            r#"{{"query": "T", "cluster": "help", "database": "Samples",
                "tables": [{{"name": "PrimaryResult", "columns": [{{"name": "n", "type": "long"}}],
                             "rows": [{}]}}]}}"#,
            rows.join(",")
        );
        let (workspace, _editor, _sent, cx) =
            setup_in(cx, 200, ANSWER, QUERIES, json!({ "many.ktt": many })).await;
        share_with(&workspace, None, Some("/root/many.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog");

        modal.read_with(cx, |modal, _| {
            let shown = modal.draft.displayed_text(modal.choice);
            assert_eq!(
                shown.lines().filter(|line| line.starts_with("| ")).count(),
                2 + DISPLAYED_ROWS
            );
            assert!(!shown.contains("rows)"), "{shown}");
            assert_eq!(modal.draft.copied_rows(modal.choice), 50);
        });
        copy_from(&modal, cx);
        let copied = clipboard_text(cx).expect("text");
        assert_eq!(
            copied.lines().filter(|line| line.starts_with("| ")).count(),
            2 + 50
        );
        assert!(!copied.contains("rows)"));
    }

    #[test]
    fn the_dialog_says_how_many_rows_are_copied_and_how_many_it_shows() {
        use ResultsFormat::{Markdown, RichText};
        assert_eq!(rows_summary(1, 1, Markdown), "Copying all 1 row.");
        assert_eq!(rows_summary(12, 12, Markdown), "Copying all 12 rows.");
        assert_eq!(rows_summary(12, 3, Markdown), "Copying 3 of 12 rows.");
        assert_eq!(
            rows_summary(50, 50, Markdown),
            "Copying all 50 rows. The preview shows the first 20."
        );
        assert_eq!(rows_summary(50, 20, Markdown), "Copying 20 of 50 rows.");
        assert_eq!(
            rows_summary(12, 3, RichText),
            "Copying 3 of 12 rows. Rich text pastes as a table, and as tab-separated rows in plain text."
        );
    }

    #[gpui::test]
    async fn the_number_of_rows_is_typed_and_a_bad_number_stops_the_copy(cx: &mut TestAppContext) {
        let (_workspace, modal, cx) = shared_result(cx).await;

        modal.update_in(cx, |modal, window, cx| modal.take_top_rows(window, cx));
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.choice.top_rows),
            Some(DEFAULT_TOP_ROWS)
        );
        let type_rows = |text: &str, cx: &mut VisualTestContext| {
            modal.update_in(cx, |modal, window, cx| {
                modal
                    .rows_editor
                    .update(cx, |editor, cx| editor.set_text(text, window, cx));
            });
        };

        type_rows(" 4 ", cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.choice.top_rows, Some(4));
            assert!(modal.can_copy());
        });

        for bad in ["", "abc", "0", "-1", "2.5"] {
            type_rows(bad, cx);
            modal.read_with(cx, |modal, _| {
                assert!(modal.rows_invalid, "{bad:?} is not a number of rows");
                assert!(!modal.can_copy());
            });
            copy_from(&modal, cx);
            assert_eq!(clipboard_text(cx), None, "{bad:?} copies nothing");
        }

        type_rows("7", cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.choice.top_rows, Some(7));
            assert!(!modal.rows_invalid);
        });
    }

    #[gpui::test]
    async fn all_rows_can_be_taken_again_and_typing_then_changes_nothing(cx: &mut TestAppContext) {
        let (_workspace, modal, cx) = shared_result(cx).await;
        modal.update_in(cx, |modal, window, cx| modal.take_top_rows(window, cx));

        modal.update(cx, |modal, cx| modal.take_all_rows(cx));
        modal.update_in(cx, |modal, window, cx| {
            modal
                .rows_editor
                .update(cx, |editor, cx| editor.set_text("nonsense", window, cx));
        });

        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.choice.top_rows, None);
            assert!(!modal.rows_invalid);
            assert!(modal.can_copy());
        });
    }

    #[gpui::test]
    async fn the_format_and_the_rows_taken_are_kept_for_next_time(cx: &mut TestAppContext) {
        let (workspace, modal, cx) = shared_result(cx).await;
        change(
            &modal,
            |choice| {
                choice.format = ResultsFormat::RichText;
                choice.top_rows = Some(5);
            },
            cx,
        );
        copy_from(&modal, cx);

        let kept = kept_choice(&workspace, cx).await;
        assert_eq!(kept["format"], json!("richText"));
        assert_eq!(kept["topRows"], json!(5));

        share_with(&workspace, None, Some("/root/shared.ktt"), cx);
        let second = self::modal(&workspace, cx).expect("the dialog asks again");
        second.read_with(cx, |modal, cx| {
            assert_eq!(modal.choice.format, ResultsFormat::RichText);
            assert_eq!(modal.choice.top_rows, Some(5));
            assert_eq!(modal.rows_editor.read(cx).text(cx), "5");
        });
    }

    #[gpui::test]
    async fn a_choice_kept_before_there_were_formats_reads_as_markdown_with_all_rows(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "shared.ktt": TWELVE_ROWS }),
        )
        .await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        fs.create_dir(choice_path().parent().expect("a folder"))
            .await
            .expect("the folder");
        fs.write(
            &choice_path(),
            br#"{"link":false,"query":true,"resultsPreview":true,"remember":false}"#,
        )
        .await
        .expect("the file is written");

        share_with(&workspace, None, Some("/root/shared.ktt"), cx);

        let modal = modal(&workspace, cx).expect("the dialog");
        modal.read_with(cx, |modal, _| {
            assert!(!modal.choice.link);
            assert_eq!(modal.choice.format, ResultsFormat::Markdown);
            assert_eq!(modal.choice.top_rows, None);
        });
    }

    #[gpui::test]
    async fn a_part_that_is_unchecked_is_not_copied(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "shared.ktt": TWELVE_ROWS }),
        )
        .await;
        share_with(&workspace, None, Some("/root/shared.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog");

        change(&modal, |choice| choice.query = false, cx);
        copy_from(&modal, cx);

        assert_eq!(
            clipboard_text(cx),
            Some(format!("{}\n\n{ALL_TWELVE}", link_to(TWELVE_ROWS_QUERY)))
        );
    }

    #[gpui::test]
    async fn the_dialog_does_not_copy_when_nothing_is_chosen(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        share_with(&workspace, Some(&editor), None, cx);
        let modal = modal(&workspace, cx).expect("the dialog");

        change(
            &modal,
            |choice| {
                choice.link = false;
                choice.query = false;
            },
            cx,
        );
        copy_from(&modal, cx);

        assert_eq!(clipboard_text(cx), None);
        assert!(self::modal(&workspace, cx).is_some(), "the dialog stays");
    }

    #[gpui::test]
    async fn a_result_that_does_not_say_where_it_ran_has_no_link_but_can_be_shared(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "plain.ktt": WITHOUT_CLUSTER }),
        )
        .await;
        share_with(&workspace, None, Some("/root/plain.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog");

        modal.read_with(cx, |modal, _| {
            assert!(modal.draft.link.is_err());
            assert_eq!(
                modal.draft.text(modal.choice),
                "StormEvents | take 1\n\n| n |\n| --- |\n| 1 |",
                "the link is left out even though it is chosen"
            );
        });
        copy_from(&modal, cx);
        assert_eq!(
            clipboard_text(cx).as_deref(),
            Some("StormEvents | take 1\n\n| n |\n| --- |\n| 1 |")
        );
    }

    #[gpui::test]
    async fn a_result_that_keeps_no_query_says_so_instead_of_asking(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ "plain.ktt": WITHOUT_QUERY }),
        )
        .await;

        share_with(&workspace, None, Some("/root/plain.ktt"), cx);

        assert!(modal(&workspace, cx).is_none());
        assert!(error_shown(&workspace, cx));
    }

    #[gpui::test]
    async fn a_file_that_is_not_there_says_so(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;

        share_with(&workspace, None, Some("/gone/result.ktt"), cx);

        assert!(modal(&workspace, cx).is_none());
        assert!(error_shown(&workspace, cx));
    }

    #[gpui::test]
    async fn with_no_query_and_no_result_there_is_nothing_to_share(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;

        share_with(&workspace, None, None, cx);

        assert!(modal(&workspace, cx).is_none());
        assert!(error_shown(&workspace, cx));
    }

    #[gpui::test]
    async fn the_dialog_starts_as_it_was_left(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        share_with(&workspace, Some(&editor), None, cx);
        let first = modal(&workspace, cx).expect("the dialog");
        assert_eq!(
            first.read_with(cx, |modal, _| modal.choice),
            ShareChoice::default(),
            "everything is chosen the first time"
        );

        change(&first, |choice| choice.link = false, cx);
        copy_from(&first, cx);
        assert_eq!(
            kept_choice(&workspace, cx).await,
            json!({
                "link": false, "query": true, "resultsPreview": true,
                "format": "markdown", "topRows": null, "remember": false
            })
        );

        share_with(&workspace, Some(&editor), None, cx);
        let second = modal(&workspace, cx).expect("the dialog asks again");
        let choice = second.read_with(cx, |modal, _| modal.choice);
        assert!(!choice.link && choice.query && !choice.remember);
    }

    #[gpui::test]
    async fn a_remembered_choice_is_shared_without_asking(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        share_with(&workspace, Some(&editor), None, cx);
        let first = modal(&workspace, cx).expect("the dialog");
        change(
            &first,
            |choice| {
                choice.link = false;
                choice.remember = true;
            },
            cx,
        );
        copy_from(&first, cx);
        assert_eq!(clipboard_text(cx).as_deref(), Some(CURSOR_QUERY));

        cx.write_to_clipboard(ClipboardItem::new_string("something else".into()));
        share_with(&workspace, Some(&editor), None, cx);

        assert!(modal(&workspace, cx).is_none(), "no dialog");
        assert_eq!(clipboard_text(cx).as_deref(), Some(CURSOR_QUERY));
    }

    #[gpui::test]
    async fn a_remembered_choice_that_leaves_nothing_to_copy_asks_again(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        save_choice(
            fs.as_ref(),
            ShareChoice {
                link: false,
                query: false,
                results_preview: true,
                format: ResultsFormat::Markdown,
                top_rows: None,
                remember: true,
            },
        )
        .await
        .expect("the choice is kept");

        share_with(&workspace, Some(&editor), None, cx);

        assert!(
            modal(&workspace, cx).is_some(),
            "a query with no rows has nothing of what was remembered"
        );
        assert_eq!(clipboard_text(cx), None);
    }

    #[gpui::test]
    async fn a_choice_file_that_cannot_be_read_is_replaced_by_the_default(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        fs.create_dir(choice_path().parent().expect("a folder"))
            .await
            .expect("the folder");
        fs.write(&choice_path(), b"not json")
            .await
            .expect("the file is written");

        share_with(&workspace, Some(&editor), None, cx);

        let modal = modal(&workspace, cx).expect("the dialog");
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.choice),
            ShareChoice::default()
        );
    }

    #[gpui::test]
    async fn a_query_with_no_text_at_the_cursor_says_so(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        editor.update_in(cx, |editor, window, cx| editor.set_text("", window, cx));

        share_with(&workspace, Some(&editor), None, cx);

        assert!(modal(&workspace, cx).is_none());
        assert!(error_shown(&workspace, cx));
    }
}
