//! Saving a query as a snippet, so that typing its prefix inserts the query again.

use std::any::TypeId;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use editor::Editor;
use fs::Fs;
use gpui::{
    App, AppContext as _, Context, DismissEvent, DispatchPhase, Entity, EventEmitter, Focusable,
    Task, WeakEntity, Window,
};
use gpui_util::ResultExt as _;
use language::LanguageName;
use snippet_provider::{AddedSnippet, add_snippet};
use ui::prelude::*;
use workspace::notifications::NotificationId;
use workspace::{ModalView, OpenOptions, OpenVisible, Toast, Workspace};
use zed_actions::kusto::SaveQueryAsSnippet;

use crate::run_query::query_text_at_cursor;

const LANGUAGE_NAME: &str = "Kusto";

/// Lets a Kusto editor handle [`SaveQueryAsSnippet`]. The handler is only there while the
/// editor shows a Kusto buffer, which is how the editor's context menu knows to offer it.
pub(crate) fn register(editor: &mut Editor) {
    editor
        .register_action_renderer(|editor, window, cx| {
            if !shows_kusto(editor, cx) {
                return;
            }
            let editor = cx.entity().downgrade();
            window.on_action(
                TypeId::of::<SaveQueryAsSnippet>(),
                move |_, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        save_query_as_snippet(&editor, window, cx);
                    }
                },
            );
        })
        .detach();
}

fn shows_kusto(editor: &Editor, cx: &App) -> bool {
    editor
        .buffer()
        .read(cx)
        .as_singleton()
        .and_then(|buffer| buffer.read(cx).language())
        .is_some_and(|language| language.name().as_ref() == LANGUAGE_NAME)
}

fn save_query_as_snippet(editor: &WeakEntity<Editor>, window: &mut Window, cx: &mut App) {
    let Some(editor) = editor.upgrade() else {
        return;
    };
    let Some(workspace) = editor.read(cx).workspace() else {
        return;
    };
    let query = editor.update(cx, |editor, cx| query_text_at_cursor(editor, cx));
    workspace.update(cx, |workspace, cx| {
        let Some(query) = query else {
            workspace.show_error(anyhow!("There is no query at the cursor."), cx);
            return;
        };
        let scope = LanguageName::new(LANGUAGE_NAME).snippet_scope_id();
        let fs = workspace.app_state().fs.clone();
        let workspace_handle = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| {
            SnippetPrefixModal::new(query, scope, fs, workspace_handle, window, cx)
        });
    });
}

/// Asks for the prefix of the snippet, which is also its name, and saves it.
struct SnippetPrefixModal {
    prefix: Entity<Editor>,
    query: String,
    scope: String,
    fs: Arc<dyn Fs>,
    workspace: WeakEntity<Workspace>,
    error: Option<SharedString>,
    /// Set while the snippet is being written, so that a second Enter does not cancel it.
    saving: Option<Task<()>>,
}

impl EventEmitter<DismissEvent> for SnippetPrefixModal {}
impl ModalView for SnippetPrefixModal {}

impl Focusable for SnippetPrefixModal {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.prefix.focus_handle(cx)
    }
}

impl SnippetPrefixModal {
    fn new(
        query: String,
        scope: String,
        fs: Arc<dyn Fs>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefix = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Prefix, such as stormbystate", window, cx);
            editor
        });
        Self {
            prefix,
            query,
            scope,
            fs,
            workspace,
            error: None,
            saving: None,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let prefix = self.prefix.read(cx).text(cx).trim().to_string();
        if prefix.is_empty() {
            self.error = Some("Give the snippet a prefix.".into());
            cx.notify();
            return;
        }
        self.error = None;
        self.saving = Some(cx.spawn_in(window, {
            let fs = self.fs.clone();
            let scope = self.scope.clone();
            let query = self.query.clone();
            async move |this, cx| {
                let added = add_snippet(
                    fs.as_ref(),
                    paths::snippets_dir(),
                    &scope,
                    &prefix,
                    &prefix,
                    &query,
                )
                .await;
                this.update(cx, |this, cx| match added {
                    Ok(AddedSnippet::Added(path)) => this.saved(&prefix, path, cx),
                    Ok(AddedSnippet::NameTaken) => {
                        this.saving = None;
                        this.error = Some(
                            format!(
                                "There is a snippet named {prefix} in {} already.",
                                this.scope
                            )
                            .into(),
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        this.saving = None;
                        this.error = Some(format!("{error:#}").into());
                        cx.notify();
                    }
                })
                .log_err();
            }
        }));
    }

    fn saved(&mut self, prefix: &str, path: PathBuf, cx: &mut Context<Self>) {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let workspace_handle = self.workspace.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<SaveQueryAsSnippet>(),
                        format!("Saved the snippet {prefix} to {file_name}"),
                    )
                    .on_click("Open", move |window, cx| {
                        workspace_handle
                            .update(cx, |workspace, cx| {
                                workspace
                                    .open_abs_path(
                                        path.clone(),
                                        OpenOptions {
                                            visible: Some(OpenVisible::None),
                                            ..Default::default()
                                        },
                                        window,
                                        cx,
                                    )
                                    .detach_and_log_err(cx)
                            })
                            .log_err();
                    })
                    .autohide(),
                    cx,
                )
            })
            .log_err();
        cx.emit(DismissEvent);
    }
}

impl Render for SnippetPrefixModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .key_context("SnippetPrefixModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w_96()
            .overflow_hidden()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(theme.colors().border_variant)
                    .child(self.prefix.clone()),
            )
            .child(
                h_flex()
                    .bg(theme.colors().editor_background)
                    .rounded_b_sm()
                    .w_full()
                    .p_2()
                    .gap_1()
                    .child(match self.error.clone() {
                        Some(error) => Label::new(error).size(LabelSize::Small).color(Color::Error),
                        None => Label::new(format!(
                            "Typing this prefix inserts the query. It is saved to {}.json.",
                            self.scope
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use multi_buffer::MultiBufferOffset;
    use serde_json::json;

    use crate::run_query::tests::{ANSWER, kusto_language, setup, setup_in};

    use super::*;

    fn set_language(
        editor: &Entity<Editor>,
        language: Option<Arc<language::Language>>,
        cx: &mut VisualTestContext,
    ) {
        editor.update(cx, |editor, cx| {
            let buffer = editor
                .buffer()
                .read(cx)
                .as_singleton()
                .expect("a file is one buffer");
            buffer.update(cx, |buffer, cx| buffer.set_language(language, cx));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn is_offered(cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| window.is_action_available(&SaveQueryAsSnippet, cx))
    }

    fn modal(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<SnippetPrefixModal>> {
        workspace.read_with(cx, |workspace, cx| {
            workspace.active_modal::<SnippetPrefixModal>(cx)
        })
    }

    fn type_prefix(modal: &Entity<SnippetPrefixModal>, prefix: &str, cx: &mut VisualTestContext) {
        modal.update_in(cx, |modal, window, cx| {
            modal
                .prefix
                .update(cx, |editor, cx| editor.set_text(prefix, window, cx));
        });
    }

    /// What Enter does in the prompt. The workspace of these tests is not rendered inside a
    /// `MultiWorkspace`, so the prompt never gets keys and the tests call its handler.
    fn confirm(modal: &Entity<SnippetPrefixModal>, cx: &mut VisualTestContext) {
        modal.update_in(cx, |modal, window, cx| {
            modal.confirm(&menu::Confirm, window, cx)
        });
        cx.run_until_parked();
    }

    async fn snippets_file(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<serde_json::Value> {
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let text = fs
            .load(&paths::snippets_dir().join("kusto.json"))
            .await
            .ok()?;
        serde_json::from_str(&text).ok()
    }

    #[gpui::test]
    async fn the_action_is_offered_in_a_kusto_buffer_only(cx: &mut TestAppContext) {
        let (_workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        assert!(!is_offered(cx), "a buffer of no language is not Kusto");

        set_language(&editor, Some(kusto_language()), cx);
        assert!(is_offered(cx));

        let other = Arc::new(language::Language::new(
            language::LanguageConfig {
                name: "Plain".into(),
                ..Default::default()
            },
            None,
        ));
        set_language(&editor, Some(other), cx);
        assert!(!is_offered(cx), "another language is not offered it");
    }

    #[gpui::test]
    async fn the_context_menu_of_a_kusto_file_opens_with_the_entry_on_offer(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        set_language(&editor, Some(kusto_language()), cx);
        assert!(is_offered(cx), "so the menu is built with its entry");

        editor.update_in(cx, |editor, window, cx| {
            editor.open_context_menu(&editor::actions::OpenContextMenu, window, cx)
        });

        assert!(editor.read_with(cx, |editor, _| editor.has_mouse_context_menu()));
    }

    #[gpui::test]
    async fn saves_the_query_at_the_cursor_under_the_prefix_given(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        set_language(&editor, Some(kusto_language()), cx);
        editor.update_in(cx, |editor, window, cx| {
            let second = editor
                .text(cx)
                .rfind("StormEvents")
                .expect("the second query");
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([MultiBufferOffset(second)..MultiBufferOffset(second)])
            });
        });

        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();
        let modal = modal(&workspace, cx).expect("the prefix is asked for");
        type_prefix(&modal, "storm count", cx);
        confirm(&modal, cx);

        assert!(
            self::modal(&workspace, cx).is_none(),
            "the prompt is gone once the snippet is saved"
        );
        let file = snippets_file(&workspace, cx)
            .await
            .expect("the file is written");
        assert_eq!(
            file["storm count"],
            json!({ "prefix": "storm count", "body": ["StormEvents", "| count"] })
        );
        assert_eq!(file.as_object().map(|snippets| snippets.len()), Some(1));
    }

    #[gpui::test]
    async fn saves_the_selection_when_there_is_one(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        set_language(&editor, Some(kusto_language()), cx);
        editor.update_in(cx, |editor, window, cx| {
            let start = editor.text(cx).find("| take 1").expect("the first query");
            let end = start + "| take 1".len();
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([MultiBufferOffset(start)..MultiBufferOffset(end)])
            });
        });

        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();
        let modal = modal(&workspace, cx).expect("the prefix is asked for");
        type_prefix(&modal, "one", cx);
        confirm(&modal, cx);

        let file = snippets_file(&workspace, cx)
            .await
            .expect("the file is written");
        assert_eq!(file["one"]["body"], json!(["| take 1"]));
    }

    #[gpui::test]
    async fn saves_dollars_and_backslashes_so_that_they_insert_as_written(cx: &mut TestAppContext) {
        const JOIN: &str =
            "T\n| join (U) on $left.Id == $right.Id\n| where P matches regex @\"\\d+\"\n";
        let (workspace, editor, _sent, cx) = setup_in(cx, 200, ANSWER, JOIN, json!({})).await;
        set_language(&editor, Some(kusto_language()), cx);

        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();
        let modal = modal(&workspace, cx).expect("the prefix is asked for");
        type_prefix(&modal, "joined", cx);
        confirm(&modal, cx);

        let file = snippets_file(&workspace, cx)
            .await
            .expect("the file is written");
        assert_eq!(
            file["joined"]["body"],
            json!([
                "T",
                "| join (U) on \\$left.Id == \\$right.Id",
                "| where P matches regex @\"\\\\d+\"",
            ]),
        );
    }

    #[gpui::test]
    async fn stays_open_with_a_reason_when_nothing_was_saved(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        set_language(&editor, Some(kusto_language()), cx);
        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();
        let modal = modal(&workspace, cx).expect("the prefix is asked for");

        confirm(&modal, cx);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.error.clone()),
            Some("Give the snippet a prefix.".into())
        );
        assert!(snippets_file(&workspace, cx).await.is_none());

        type_prefix(&modal, "first", cx);
        confirm(&modal, cx);
        assert!(self::modal(&workspace, cx).is_none());

        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();
        let modal = self::modal(&workspace, cx).expect("asked for again");
        type_prefix(&modal, "first", cx);
        confirm(&modal, cx);
        assert_eq!(
            modal.read_with(cx, |modal, _| modal.error.clone()),
            Some("There is a snippet named first in kusto already.".into()),
            "the name is taken, and the one saved is not replaced"
        );
        assert!(self::modal(&workspace, cx).is_some());

        type_prefix(&modal, "second", cx);
        confirm(&modal, cx);
        assert!(
            self::modal(&workspace, cx).is_none(),
            "another prefix in the same prompt is saved"
        );
        let file = snippets_file(&workspace, cx)
            .await
            .expect("the file is written");
        assert_eq!(file.as_object().map(|snippets| snippets.len()), Some(2));
    }

    #[gpui::test]
    async fn asks_for_nothing_when_there_is_no_query(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) =
            setup_in(cx, 200, ANSWER, "// only a note\n", json!({})).await;
        set_language(&editor, Some(kusto_language()), cx);

        cx.dispatch_action(SaveQueryAsSnippet);
        cx.run_until_parked();

        assert!(modal(&workspace, cx).is_none());
        assert!(snippets_file(&workspace, cx).await.is_none());
    }
}
