//! The editor of a query thread: a Kusto query file kept in the user data directory, whose
//! queries run into tabs.
//!
//! The editor is in no pane, so nothing saves it. The thread saves its own file shortly after
//! each edit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use editor::Editor;
use fs::{CreateOptions, Fs};
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Task, WeakEntity, Window,
    div,
};
use gpui_util::ResultExt as _;
use language::{Buffer, BufferEvent};
use project::Project;
use workspace::Workspace;

use crate::query_parameters::{SelectParameterProfile, select_thread_parameter_profile};
use crate::run_query::{
    CancelQuery, CopyQuery, RunQuery, ShowResult, cancel_thread_query, copy_thread_query,
    run_thread_query, show_thread_result,
};

const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Where the query files of threads are kept.
pub fn threads_folder() -> PathBuf {
    paths::data_dir().join("kusto").join("threads")
}

pub struct QueryThread {
    workspace: WeakEntity<Workspace>,
    project: WeakEntity<Project>,
    path: PathBuf,
    buffer: Entity<Buffer>,
    editor: Entity<Editor>,
    save: Option<Task<()>>,
    _buffer_subscription: Subscription,
}

impl QueryThread {
    /// Opens the query file at `path`, making it and its folder first when they do not exist.
    ///
    /// The workspace is passed as a handle, and its project and file system beside it, because a
    /// thread is usually made from inside an update of the workspace, which cannot be read then.
    pub fn open(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        fs: Arc<dyn Fs>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        window.spawn(cx, async move |cx| {
            if let Some(folder) = path.parent() {
                fs.create_dir(folder)
                    .await
                    .with_context(|| format!("could not make {}", folder.display()))?;
            }
            fs.create_file(
                &path,
                CreateOptions {
                    overwrite: false,
                    ignore_if_exists: true,
                },
            )
            .await
            .with_context(|| format!("could not make {}", path.display()))?;
            let buffer = project
                .update(cx, |project, cx| project.open_local_buffer(&path, cx))
                .await
                .with_context(|| format!("could not open {}", path.display()))?;
            cx.update(|window, cx| {
                cx.new(|cx| Self::new(workspace, project, path, buffer, window, cx))
            })
        })
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        path: PathBuf,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx);
            // No pane adds this editor to the workspace, and a lens does nothing without one.
            editor.set_workspace(workspace.clone());
            editor
        });
        let buffer_subscription = cx.subscribe(&buffer, |this, _, event, cx| {
            if matches!(event, BufferEvent::Edited { .. }) {
                this.save_soon(cx);
            }
        });
        Self {
            workspace,
            project: project.downgrade(),
            path,
            buffer,
            editor,
            save: None,
            _buffer_subscription: buffer_subscription,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    /// Replaces an unfinished save, so that a burst of typing is one write.
    fn save_soon(&mut self, cx: &mut Context<Self>) {
        let buffer = self.buffer.clone();
        let project = self.project.clone();
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let saved = match project.update(cx, |project, cx| project.save_buffer(buffer, cx)) {
                Ok(saving) => saving.await,
                Err(error) => Err(error),
            };
            if let Err(error) = saved {
                this.update(cx, |this, cx| this.show_error(error, cx)).log_err();
            }
        }));
    }

    fn show_error(&self, error: anyhow::Error, cx: &mut Context<Self>) {
        let error = error.context(format!("could not save {}", self.path.display()));
        self.workspace
            .update(cx, |workspace, cx| workspace.show_error(error, cx))
            .log_err();
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            run_thread_query(&workspace, &self.editor, window, cx);
        }
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            copy_thread_query(&workspace, &self.editor, window, cx);
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            cancel_thread_query(&workspace, &self.editor, cx);
        }
    }

    fn show_result(&mut self, action: &ShowResult, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            show_thread_result(&workspace, PathBuf::from(&action.path), window, cx);
        }
    }

    fn select_parameter_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            select_thread_parameter_profile(&workspace, &self.editor, window, cx);
        }
    }
}

impl Focusable for QueryThread {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for QueryThread {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("KustoQueryThread")
            .size_full()
            .on_action(cx.listener(|this, _: &RunQuery, window, cx| this.run(window, cx)))
            .on_action(cx.listener(|this, _: &CancelQuery, _, cx| this.cancel(cx)))
            .on_action(cx.listener(|this, _: &CopyQuery, window, cx| this.copy(window, cx)))
            .on_action(cx.listener(|this, action: &ShowResult, window, cx| {
                this.show_result(action, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectParameterProfile, window, cx| {
                this.select_parameter_profile(window, cx)
            }))
            .child(self.editor.clone())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;

    use super::*;
    use crate::results_panel::ResultsPanel;
    use crate::run_query::tests::{ANSWER, QUERIES, Sent, bodies, results_viewers, setup, setup_in};

    async fn open(
        workspace: &Entity<Workspace>,
        name: &str,
        cx: &mut VisualTestContext,
    ) -> Entity<QueryThread> {
        let path = PathBuf::from(format!("/data/kusto/threads/{name}.kql"));
        let (project, fs) = workspace.read_with(cx, |workspace, _| {
            (workspace.project().clone(), workspace.app_state().fs.clone())
        });
        cx.update(|window, cx| {
            QueryThread::open(workspace.downgrade(), project, fs, path, window, cx)
        })
        .await
        .expect("the thread opens")
    }

    fn write(thread: &Entity<QueryThread>, text: &str, cx: &mut VisualTestContext) {
        let editor = thread.read_with(cx, |thread, _| thread.editor().clone());
        editor.update_in(cx, |editor, window, cx| editor.set_text(text, window, cx));
    }

    fn put_cursor_at_the_start(thread: &Entity<QueryThread>, cx: &mut VisualTestContext) {
        let editor = thread.read_with(cx, |thread, _| thread.editor().clone());
        editor.update_in(cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([text::Point::new(0, 0)..text::Point::new(0, 0)])
            })
        });
    }

    fn run(thread: &Entity<QueryThread>, cx: &mut VisualTestContext) {
        thread.update_in(cx, |thread, window, cx| thread.run(window, cx));
        cx.run_until_parked();
    }

    fn queries_sent(sent: &Sent) -> Vec<String> {
        bodies(sent, "/v2/rest/query")
            .iter()
            .map(|body| {
                let body: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
                body["csl"].as_str().expect("a query").to_string()
            })
            .collect()
    }

    #[gpui::test]
    async fn a_new_thread_makes_its_file_and_opens_it_empty(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        assert!(!fs.is_file(Path::new("/data/kusto/threads/one.kql")).await);

        let thread = open(&workspace, "one", cx).await;

        assert!(fs.is_file(Path::new("/data/kusto/threads/one.kql")).await);
        let editor = thread.read_with(cx, |thread, _| thread.editor().clone());
        assert_eq!(editor.read_with(cx, |editor, cx| editor.text(cx)), "");
        assert_eq!(
            thread.read_with(cx, |thread, _| thread.path().to_path_buf()),
            PathBuf::from("/data/kusto/threads/one.kql")
        );
    }

    #[gpui::test]
    async fn a_thread_opened_again_has_the_text_it_had(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        fs.as_fake()
            .insert_tree(
                "/data/kusto/threads",
                json!({ "one.kql": "StormEvents\n| count\n" }),
            )
            .await;

        let thread = open(&workspace, "one", cx).await;

        let editor = thread.read_with(cx, |thread, _| thread.editor().clone());
        assert_eq!(
            editor.read_with(cx, |editor, cx| editor.text(cx)),
            "StormEvents\n| count\n"
        );
    }

    #[gpui::test]
    async fn a_thread_saves_its_file_shortly_after_an_edit(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let thread = open(&workspace, "one", cx).await;
        let path = Path::new("/data/kusto/threads/one.kql");

        write(&thread, "StormEvents", cx);
        write(&thread, "StormEvents\n| take 5", cx);
        cx.run_until_parked();
        assert_eq!(
            fs.load(path).await.expect("the file is there"),
            "",
            "nothing is written while the edits are coming"
        );

        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();

        assert_eq!(
            fs.load(path).await.expect("the file is there"),
            "StormEvents\n| take 5"
        );
    }

    #[gpui::test]
    async fn copying_from_a_thread_copies_the_query_of_that_thread(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let thread = open(&workspace, "one", cx).await;
        write(&thread, "Traces\n| take 1", cx);

        thread.update_in(cx, |thread, window, cx| thread.copy(window, cx));
        cx.run_until_parked();

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
            Some(
                "// :setDefaultCluster('https://help.kusto.windows.net')\n// :setDefaultDb('Samples')\nTraces\n| take 1"
            )
        );
    }

    #[gpui::test]
    async fn a_run_from_a_thread_opens_a_tab_whatever_the_setting_says(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 200, ANSWER).await;
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        let thread = open(&workspace, "one", cx).await;
        write(&thread, "Traces\n| take 1", cx);

        run(&thread, cx);

        assert_eq!(queries_sent(&sent), ["Traces\n| take 1"]);
        assert_eq!(results_viewers(&workspace, cx), 1, "the result is a tab");
        assert!(
            panel.read_with(cx, |panel, _| panel.shown_viewer().is_none()),
            "the panel shows nothing"
        );

        run(&thread, cx);

        assert_eq!(
            results_viewers(&workspace, cx),
            2,
            "each run opens its own tab"
        );
    }

    #[gpui::test]
    async fn a_threads_editor_acts_on_the_workspace(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let thread = open(&workspace, "one", cx).await;

        let editor = thread.read_with(cx, |thread, _| thread.editor().clone());

        assert_eq!(
            editor.read_with(cx, |editor, _| editor.workspace()),
            Some(workspace),
            "a lens does nothing in an editor without one"
        );
    }

    #[gpui::test]
    async fn a_result_a_lens_names_opens_in_a_tab_whatever_the_setting_says(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let thread = open(&workspace, "one", cx).await;
        write(&thread, "Traces\n| take 1", cx);
        run(&thread, cx);
        assert_eq!(results_viewers(&workspace, cx), 1);

        // The result of the run is already in a tab, which showing it again would only
        // bring forward, so the lens names a copy.
        let history = crate::history::history_folder();
        let mut saved = fs.read_dir(&history).await.expect("the history folder");
        let result = futures::StreamExt::next(&mut saved)
            .await
            .expect("the run was saved")
            .expect("the file is listed");
        let copy = PathBuf::from("/data/an-earlier-run.ktt");
        fs.copy_file(&result, &copy, fs::CopyOptions::default())
            .await
            .expect("the result is copied");
        thread.update_in(cx, |thread, window, cx| {
            thread.show_result(
                &ShowResult {
                    path: copy.to_string_lossy().into_owned(),
                },
                window,
                cx,
            )
        });
        cx.run_until_parked();

        assert_eq!(results_viewers(&workspace, cx), 2, "the earlier result is a tab");
        assert!(
            panel.read_with(cx, |panel, _| panel.shown_viewer().is_none()),
            "the panel shows nothing"
        );
    }

    #[gpui::test]
    async fn a_run_from_a_thread_runs_only_the_query_of_that_thread(cx: &mut TestAppContext) {
        let (workspace, file_editor, sent, cx) = setup(cx, 200, ANSWER).await;
        assert!(file_editor.read_with(cx, |editor, cx| editor.text(cx)) == QUERIES);
        let first = open(&workspace, "first", cx).await;
        let second = open(&workspace, "second", cx).await;
        write(&first, "First\n| take 1", cx);
        write(&second, "Second\n| take 2\n\nSecond\n| count", cx);
        put_cursor_at_the_start(&second, cx);

        run(&second, cx);

        assert_eq!(queries_sent(&sent), ["Second\n| take 2"]);
    }

    #[gpui::test]
    async fn a_run_from_a_thread_takes_its_values_from_the_projects_active_profile(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, sent, cx) = setup_in(
            cx,
            200,
            ANSWER,
            QUERIES,
            json!({ ".kusto": { "parameters.yaml":
                "active: B\nprofiles:\n  A:\n    raid: from-a\n  B:\n    raid: from-b\n" } }),
        )
        .await;
        let thread = open(&workspace, "one", cx).await;
        write(
            &thread,
            "declare query_parameters(raid:string);\nStormEvents\n| where Id == raid",
            cx,
        );

        run(&thread, cx);

        let sent = bodies(&sent, "/v2/rest/query");
        assert_eq!(sent.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&sent[0]).expect("a JSON body");
        assert_eq!(
            body["properties"],
            json!({ "Parameters": { "raid": "from-b" } })
        );
    }
}
