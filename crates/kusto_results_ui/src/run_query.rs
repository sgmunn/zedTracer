//! Running the query under the cursor, and showing what it returned.
//!
//! A run is written to a `.ktt` file in the history folder and opened like any other results
//! file, so a result belongs to the tab that was opened for it and a late answer can never
//! replace another run's.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use chrono::Utc;
use editor::Editor;
use fs::Fs;
use gpui::{App, AsyncWindowContext, AppContext as _, BackgroundExecutor, Context, Entity, Task, TaskExt as _, Window, actions};
use gpui_util::ResultExt as _;
use kusto_client::{
    AzureCliTokenProvider, Cluster, KustoClient, QueryRequest, TokenProvider, query_range_at,
};
use multi_buffer::MultiBufferOffset;
use settings::{RegisterSetting, Settings};
use workspace::notifications::{DetachAndPromptErr as _, NotificationId};
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

actions!(
    kusto,
    [
        /// Runs the selected text, or the query around the cursor, on the configured cluster.
        RunQuery
    ]
);

/// Where queries run.
#[derive(Clone, Debug, RegisterSetting)]
pub struct KustoSettings {
    pub cluster: Option<String>,
    pub database: Option<String>,
}

impl Settings for KustoSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let kusto = content.kusto.as_ref();
        let text = |pick: fn(&settings::KustoSettingsContent) -> &Option<String>| {
            kusto
                .and_then(|kusto| pick(kusto).as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        Self {
            cluster: text(|kusto| &kusto.cluster),
            database: text(|kusto| &kusto.database),
        }
    }
}

pub(crate) fn register(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let runs = cx.new(|_| QueryRuns::default());
    workspace.register_action(move |workspace, _: &RunQuery, window, cx| {
        run_query(workspace, &runs, window, cx)
    });
}

fn run_query(
    workspace: &mut Workspace,
    runs: &Entity<QueryRuns>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Err(error) = start_run(workspace, runs, window, cx) {
        workspace.show_error(error, cx);
    }
}

#[derive(Default)]
struct QueryRuns {
    next_id: usize,
    active: Vec<ActiveRun>,
}

struct ActiveRun {
    id: usize,
    request: QueryRequest,
    /// Known once the shell environment is loaded. A run cancelled before then sent nothing.
    client: Option<Arc<KustoClient>>,
    task: Task<()>,
}

impl QueryRuns {
    fn finish(&mut self, id: usize) {
        self.active.retain(|run| run.id != id);
    }

    fn set_client(&mut self, id: usize, client: Arc<KustoClient>) {
        if let Some(run) = self.active.iter_mut().find(|run| run.id == id) {
            run.client = Some(client);
        }
    }

    /// Stops waiting for the run and tells the service to stop working on it. The cancelled
    /// run shows nothing: no result and no error.
    fn cancel(&mut self, id: usize, cx: &mut App) {
        let Some(index) = self.active.iter().position(|run| run.id == id) else {
            return;
        };
        let ActiveRun {
            client,
            request,
            task,
            ..
        } = self.active.remove(index);
        drop(task);
        if let Some(client) = client {
            cx.background_spawn(async move { client.cancel(&request).await })
                .detach_and_log_err(cx);
        }
    }
}

fn query_to_run(editor: &mut Editor, cx: &mut Context<Editor>) -> Option<String> {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let display_snapshot = editor.display_snapshot(cx);
    let selection = editor
        .selections
        .newest::<MultiBufferOffset>(&display_snapshot);
    let text = snapshot.text();
    let range = if selection.is_empty() {
        query_range_at(&text, selection.head().0)?
    } else {
        selection.start.0..selection.end.0
    };
    let query = text.get(range)?.trim();
    (!query.is_empty()).then(|| query.to_string())
}

fn start_run(
    workspace: &mut Workspace,
    runs: &Entity<QueryRuns>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let editor = workspace
        .active_item_as::<Editor>(cx)
        .context("Open a Kusto query to run it.")?;
    let query = editor
        .update(cx, |editor, cx| query_to_run(editor, cx))
        .context("There is no query at the cursor.")?;

    let settings = KustoSettings::get_global(cx);
    let cluster = settings
        .cluster
        .as_deref()
        .context("Set `kusto.cluster` in your settings to the cluster to run queries on.")?;
    let cluster = Cluster::parse(cluster)?;
    let database = settings
        .database
        .clone()
        .context("Set `kusto.database` in your settings to the database to run queries in.")?;

    let run_uuid = uuid::Uuid::new_v4();
    let request = QueryRequest {
        cluster,
        database,
        query,
        client_request_id: format!("ZedTracer;{run_uuid}"),
    };

    let run_id = runs.update(cx, |runs, _| {
        runs.next_id += 1;
        runs.next_id
    });
    let toast_id = NotificationId::composite::<QueryRuns>(run_id);
    let environment = workspace.project().read(cx).environment().clone();
    let environment =
        environment.update(cx, |environment, cx| environment.default_environment(cx));
    let fs = workspace.app_state().fs.clone();
    let http_client = cx.http_client();

    workspace.show_toast(
        Toast::new(
            toast_id.clone(),
            format!("Running query on {}…", request.cluster.host()),
        )
        .on_click("Cancel", {
            let runs = runs.downgrade();
            let workspace = cx.weak_entity();
            let toast_id = toast_id.clone();
            move |_, cx| {
                runs.update(cx, |runs, cx| runs.cancel(run_id, cx)).log_err();
                workspace
                    .update(cx, |workspace, cx| workspace.dismiss_toast(&toast_id, cx))
                    .log_err();
            }
        }),
        cx,
    );

    let task = cx.spawn_in(window, {
        let runs = runs.downgrade();
        let request = request.clone();
        async move |workspace, cx| {
            let environment = environment.await.unwrap_or_default();
            let client = Arc::new(KustoClient::new(
                http_client,
                token_provider(environment, cx),
            ));
            runs.update(cx, |runs, _| runs.set_client(run_id, client.clone()))
                .log_err();
            let outcome = save_run(
                client,
                request,
                fs,
                run_uuid,
                cx.background_executor().clone(),
            )
            .await;
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.dismiss_toast(&toast_id, cx);
                    match outcome {
                        Ok(path) => workspace
                            .open_abs_path(
                                path,
                                OpenOptions {
                                    visible: Some(OpenVisible::None),
                                    ..Default::default()
                                },
                                window,
                                cx,
                            )
                            .detach_and_prompt_err(
                                "Could not open the query results",
                                window,
                                cx,
                                |_, _, _| None,
                            ),
                        Err(error) => workspace.show_error(error, cx),
                    }
                })
                .log_err();
            runs.update(cx, |runs, _| runs.finish(run_id)).log_err();
        }
    });
    runs.update(cx, |runs, _| {
        runs.active.push(ActiveRun {
            id: run_id,
            request,
            client: None,
            task,
        })
    });
    Ok(())
}

fn token_provider(
    environment: impl IntoIterator<Item = (String, String)>,
    cx: &mut AsyncWindowContext,
) -> Arc<dyn TokenProvider> {
    #[cfg(test)]
    if let Some(provider) = cx
        .update(|_, cx| cx.try_global::<tests::TestTokenProvider>().map(|global| global.0.clone()))
        .ok()
        .flatten()
    {
        return provider;
    }
    #[cfg(not(test))]
    let _ = cx;
    Arc::new(AzureCliTokenProvider::new(environment))
}

/// Runs the query and writes its result to the history folder, returning the file.
async fn save_run(
    client: Arc<KustoClient>,
    request: QueryRequest,
    fs: Arc<dyn Fs>,
    run_uuid: uuid::Uuid,
    executor: BackgroundExecutor,
) -> Result<PathBuf> {
    let json = executor
        .spawn(async move {
            let result = client.execute(&request).await?;
            result.to_json()
        })
        .await?;

    let history = paths::data_dir().join("kusto").join("history");
    fs.create_dir(&history)
        .await
        .context("could not create the history folder")?;
    let path = history.join(format!(
        "{}-{run_uuid}.ktt",
        Utc::now().format("%Y%m%d-%H%M%S")
    ));
    fs.write(&path, json.as_bytes())
        .await
        .with_context(|| format!("could not save the result to {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fs::FakeFs;
    use futures::future::BoxFuture;
    use gpui::{BorrowAppContext as _, Focusable as _, Global, TestAppContext};
    use http_client::{AsyncBody, FakeHttpClient, Response};
    use kusto_client::AccessToken;
    use project::{Project, ProjectPath};
    use serde_json::json;
    use settings::SettingsStore;
    use util::rel_path::RelPath;
    use workspace::AppState;

    use crate::ResultsViewer;

    use super::*;

    pub(super) struct TestTokenProvider(pub(super) Arc<dyn TokenProvider>);

    impl Global for TestTokenProvider {}

    struct FixedToken;

    impl TokenProvider for FixedToken {
        fn token(&self, _resource: &str) -> BoxFuture<'static, Result<kusto_client::AccessToken>> {
            Box::pin(async { Ok(AccessToken::new("secret")) })
        }
    }

    const QUERIES: &str = "StormEvents\n| take 1\n\nStormEvents\n| count\n";

    const ANSWER: &str = r#"[{"FrameType":"DataTable","TableKind":"PrimaryResult","TableName":"PrimaryResult",
        "Columns":[{"ColumnName":"n","ColumnType":"long"}],"Rows":[[1],[2]]},
        {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}]"#;

    /// Every request the fake service received, as its path and body.
    type Sent = Arc<Mutex<Vec<(String, String)>>>;

    fn bodies(sent: &Sent, path: &str) -> Vec<String> {
        sent.lock()
            .expect("test lock")
            .iter()
            .filter(|(sent_path, _)| sent_path == path)
            .map(|(_, body)| body.clone())
            .collect()
    }

    /// A workspace with `queries.kql` open, a fake Kusto service, and the sent query bodies.
    async fn setup<'a>(
        cx: &'a mut TestAppContext,
        status: u16,
        answer: &'static str,
    ) -> (
        Entity<Workspace>,
        Entity<Editor>,
        Sent,
        &'a mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
            cx.set_global(TestTokenProvider(Arc::new(FixedToken)));
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{ "kusto": { "cluster": "https://help.kusto.windows.net", "database": "Samples" } }"#,
                        cx,
                    )
                    .expect("the user settings parse");
            });
        });
        let sent: Sent = Arc::default();
        let recorded = sent.clone();
        cx.update(|cx| {
            cx.set_http_client(FakeHttpClient::create(move |request| {
                let recorded = recorded.clone();
                async move {
                    let path = request.uri().path().to_string();
                    let mut body = String::new();
                    futures::AsyncReadExt::read_to_string(&mut request.into_body(), &mut body)
                        .await?;
                    recorded
                        .lock()
                        .expect("test lock")
                        .push((path.clone(), body));
                    if status == 0 && path == "/v2/rest/query" {
                        futures::future::pending::<()>().await;
                    }
                    let body = if path.starts_with("/v1/rest/auth") {
                        r#"{"AzureAD":{"KustoServiceResourceId":"https://kusto.kusto.windows.net"}}"#
                    } else {
                        answer
                    };
                    Ok(Response::builder()
                        .status(if path.starts_with("/v1/rest/auth") || status == 0 { 200 } else { status })
                        .body(AsyncBody::from(body.to_string()))?)
                }
            }));
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "queries.kql": QUERIES })).await;
        let project = Project::test(fs, ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let path = ProjectPath {
            worktree_id,
            path: RelPath::new(std::path::Path::new("queries.kql"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let item = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("the query file opens");
        let editor = item.downcast::<Editor>().expect("the file opens in an editor");
        editor.update_in(cx, |editor, window, cx| {
            window.focus(&editor.focus_handle(cx), cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (workspace, editor, sent, cx)
    }

    /// Runs the query and lets it finish. The registered action handler is only attached when a
    /// `MultiWorkspace` renders the workspace, so the tests call what the handler calls. The runs
    /// entity must outlive the run, because dropping it cancels the run.
    fn run(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) {
        let runs = cx.new(|_| QueryRuns::default());
        start(workspace, &runs, cx);
    }

    fn start(
        workspace: &Entity<Workspace>,
        runs: &Entity<QueryRuns>,
        cx: &mut gpui::VisualTestContext,
    ) {
        workspace.update_in(cx, |workspace, window, cx| {
            run_query(workspace, runs, window, cx)
        });
        cx.run_until_parked();
    }

    fn results_viewers(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .items_of_type::<ResultsViewer>(cx)
                .count()
        })
    }

    #[gpui::test]
    async fn runs_the_query_at_the_cursor_and_opens_the_result(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 200, ANSWER).await;

        run(&workspace, cx);
        cx.run_until_parked();

        let sent = bodies(&sent, "/v2/rest/query");
        assert_eq!(sent.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&sent[0]).expect("a JSON body");
        assert_eq!(body["db"], "Samples");
        assert_eq!(body["csl"], "StormEvents\n| take 1");

        assert_eq!(results_viewers(&workspace, cx), 1);
        let viewer = workspace
            .read_with(cx, |workspace, cx| {
                workspace.items_of_type::<ResultsViewer>(cx).next()
            })
            .expect("a results tab");
        let result = viewer.read_with(cx, |viewer, cx| viewer.result(cx));
        assert_eq!(result.total_rows(), 2);
        assert!(
            !workspace.read_with(cx, |workspace, _| {
                workspace.has_notification(&NotificationId::composite::<QueryRuns>(1))
            }),
            "the running notice is gone"
        );
    }

    #[gpui::test]
    async fn runs_the_selected_text_when_there_is_a_selection(cx: &mut TestAppContext) {
        let (_workspace, editor, sent, cx) = setup(cx, 200, ANSWER).await;
        editor.update_in(cx, |editor, window, cx| {
            editor.select_all(&editor::actions::SelectAll, window, cx)
        });

        run(&_workspace, cx);
        cx.run_until_parked();

        let sent = bodies(&sent, "/v2/rest/query");
        let body: serde_json::Value = serde_json::from_str(&sent[0]).expect("a JSON body");
        assert_eq!(body["csl"], QUERIES.trim_end());
    }

    #[gpui::test]
    async fn a_failed_query_shows_an_error_and_opens_no_result(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(
            cx,
            400,
            r#"{"error":{"message":"outer","innererror":{"@message":"Semantic error: SEM0100"}}}"#,
        )
        .await;

        run(&workspace, cx);
        cx.run_until_parked();

        assert_eq!(results_viewers(&workspace, cx), 0);
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<anyhow::Error>())
        }));
    }

    #[gpui::test]
    async fn a_cancelled_run_stops_on_the_service_and_shows_nothing(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 0, ANSWER).await;
        let runs = cx.new(|_| QueryRuns::default());
        start(&workspace, &runs, cx);
        assert_eq!(bodies(&sent, "/v2/rest/query").len(), 1);
        assert!(bodies(&sent, "/v1/rest/mgmt").is_empty());

        runs.update(cx, |runs, cx| runs.cancel(1, cx));
        cx.run_until_parked();

        let cancels = bodies(&sent, "/v1/rest/mgmt");
        assert_eq!(cancels.len(), 1);
        assert!(cancels[0].contains(".cancel query"), "{}", cancels[0]);
        assert_eq!(results_viewers(&workspace, cx), 0);
        assert!(!workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<anyhow::Error>())
        }));
        assert!(runs.read_with(cx, |runs, _| runs.active.is_empty()));
    }

    #[gpui::test]
    async fn asks_for_a_cluster_when_none_is_set(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 200, ANSWER).await;
        cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(r#"{ "kusto": { "database": "Samples" } }"#, cx)
                    .expect("the user settings parse");
            });
        });

        run(&workspace, cx);
        cx.run_until_parked();

        assert!(bodies(&sent, "/v2/rest/query").is_empty());
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<anyhow::Error>())
        }));
    }

    #[gpui::test]
    async fn the_history_file_reads_back_as_the_result(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        cx.run_until_parked();

        let viewer = workspace
            .read_with(cx, |workspace, cx| {
                workspace.items_of_type::<ResultsViewer>(cx).next()
            })
            .expect("a results tab");
        let result = viewer.read_with(cx, |viewer, cx| viewer.result(cx));
        assert_eq!(result.query.as_deref(), Some("StormEvents\n| take 1"));
        assert_eq!(result.cluster.as_deref(), Some("help.kusto.windows.net"));
        assert_eq!(result.database.as_deref(), Some("Samples"));
        assert!(
            result
                .client_request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("ZedTracer;"))
        );
    }

    #[test]
    fn settings_ignore_blank_values() {
        let mut content = settings::SettingsContent::default();
        content.kusto = Some(settings::KustoSettingsContent {
            cluster: Some("  https://help.kusto.windows.net ".into()),
            database: Some("   ".into()),
        });
        let settings = KustoSettings::from_settings(&content);
        assert_eq!(
            settings.cluster.as_deref(),
            Some("https://help.kusto.windows.net")
        );
        assert_eq!(settings.database, None);
    }
}
