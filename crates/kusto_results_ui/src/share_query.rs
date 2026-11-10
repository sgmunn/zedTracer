//! Sharing a query: a dialog that asks which of a link to the query, the query itself and the first
//! rows of its result to copy, as the Azure Data Explorer web UI does, and puts them on the
//! clipboard as plain text.
//!
//! The query and where it ran come from a result file when there is one, which also holds the
//! rows, and otherwise from the query around the cursor, written as a copy writes it.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use fs::Fs;
use gpui::{
    Action, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, SharedString, Task, WeakEntity, Window, rems,
};
use gpui_util::ResultExt as _;
use kusto_client::{Shared, results_preview, share_text, web_explorer_link};
use kusto_results::ResultSet;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::prelude::*;
use ui::{Checkbox, ToggleState};
use workspace::notifications::NotificationId;
use workspace::{ModalView, Toast, Workspace};

use crate::results_panel::ResultsPanel;
use crate::results_viewer::ResultsViewer;
use crate::run_query::{PortableQueryAtCursor, load_result_file, portable_query_at_cursor};

/// Copies a link that opens a query in Azure Data Explorer, the query, and the first rows of its
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
    remember: bool,
}

impl Default for ShareChoice {
    fn default() -> Self {
        Self {
            link: true,
            query: true,
            results_preview: true,
            remember: false,
        }
    }
}

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
    results_preview: Option<String>,
}

impl Draft {
    async fn new(
        query: String,
        cluster: Option<&str>,
        database: Option<&str>,
        results_preview: Option<String>,
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
            results_preview,
        }
    }

    async fn of_result(result: &ResultSet) -> Result<Self> {
        let query = result
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .context("This result does not keep its query, so there is nothing to share.")?;
        Ok(Self::new(
            query.to_string(),
            result.cluster.as_deref(),
            result.database.as_deref(),
            result.tables.first().and_then(results_preview),
        )
        .await)
    }

    /// The text of the parts that were chosen and that there are.
    fn text(&self, choice: ShareChoice) -> String {
        share_text(Shared {
            link: self.link.as_deref().ok().filter(|_| choice.link),
            query: choice.query.then_some(self.query.as_str()),
            results_preview: self
                .results_preview
                .as_deref()
                .filter(|_| choice.results_preview),
        })
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
    workspace.toggle_modal(window, cx, move |_, cx| {
        ShareModal::new(draft, choice, workspace_handle, cx)
    });
}

fn copy(
    workspace: &mut Workspace,
    draft: &Draft,
    choice: ShareChoice,
    cx: &mut Context<Workspace>,
) {
    cx.write_to_clipboard(ClipboardItem::new_string(draft.text(choice)));
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
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            draft,
            choice,
            workspace,
            focus_handle: cx.focus_handle(),
        }
    }

    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.draft.query
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if self.draft.text(self.choice).is_empty() {
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

impl Render for ShareModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.draft.text(self.choice);
        let choice = self.choice;
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
                .results_preview
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
            .w(rems(44.))
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
                            .w_40()
                            .gap_2()
                            .child(link)
                            .child(query)
                            .child(results),
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
                                .disabled(text.is_empty())
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
    const TEN_OF_TWELVE: &str = "n\n0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n(10 of 12 rows)";

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
            assert_eq!(modal.draft.results_preview, None, "the query was not run");
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
    async fn the_first_rows_of_a_result_are_shared_with_its_query_and_link(
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

        share_with(&workspace, None, Some("/root/shared.ktt"), cx);
        let modal = modal(&workspace, cx).expect("the dialog asks what to share");
        copy_from(&modal, cx);

        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "{}\n\n{TWELVE_ROWS_QUERY}\n\n{TEN_OF_TWELVE}",
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
            assert_eq!(modal.draft.results_preview.as_deref(), Some(TEN_OF_TWELVE));
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
            Some(format!("{}\n\n{TEN_OF_TWELVE}", link_to(TWELVE_ROWS_QUERY)))
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
                "StormEvents | take 1\n\nn\n1",
                "the link is left out even though it is chosen"
            );
        });
        copy_from(&modal, cx);
        assert_eq!(
            clipboard_text(cx).as_deref(),
            Some("StormEvents | take 1\n\nn\n1")
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
            json!({ "link": false, "query": true, "resultsPreview": true, "remember": false })
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
