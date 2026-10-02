//! Running the query under the cursor, and showing what it returned.
//!
//! A run is written to a `.ktt` file in the history folder and opened like any other results
//! file, so a result belongs to the tab that was opened for it and a late answer can never
//! replace another run's.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use chrono::{SecondsFormat, Utc};
use editor::Editor;
use fs::Fs;
use gpui::{
    Action, App, AppContext as _, AsyncWindowContext, BackgroundExecutor, ClipboardItem, Context,
    Entity, Task, TaskExt as _, WeakEntity, Window, actions,
};
use gpui_util::ResultExt as _;
use kusto_client::{
    AzureCliTokenProvider, Cluster, Connection, DEFAULTS_FILE, KustoClient, QueryRequest,
    RUN_LOG_FILE,
    RunRecord, TokenProvider, append_record, connection_for_selection, resolve_query_at,
};
use schemars::JsonSchema;
use serde::Deserialize;
use multi_buffer::MultiBufferOffset;
use project::{ProjectItem as _, ProjectPath};
use settings::{KustoResultsLocation, RegisterSetting, Settings, SettingsStore};
use workspace::notifications::{DetachAndPromptErr as _, NotificationId};
use workspace::{OpenOptions, OpenVisible, Toast, Workspace};

use crate::results_panel::ResultsPanel;
use crate::results_viewer::ResultsFile;

actions!(
    kusto,
    [
        /// Runs the selected text, or the query around the cursor, on the configured cluster.
        RunQuery,
        /// Cancels the running query around the cursor, or the newest running query.
        CancelQuery
    ]
);

/// Shows a saved result, such as one from the history folder, in the Results panel.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = kusto)]
#[serde(deny_unknown_fields)]
pub struct ShowResult {
    pub path: String,
}

/// Copies the client request id of a run, which names it to the service and to support.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = kusto)]
#[serde(deny_unknown_fields)]
pub struct CopyClientRequestId {
    pub id: String,
}

/// Where queries run.
#[derive(Clone, Debug, RegisterSetting)]
pub struct KustoSettings {
    pub cluster: Option<String>,
    pub database: Option<String>,
    pub results_location: KustoResultsLocation,
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
            results_location: kusto
                .and_then(|kusto| kusto.results_location)
                .unwrap_or(KustoResultsLocation::Panel),
        }
    }
}

/// Keeps the language server's idea of the default cluster and database the same as the one runs
/// use, so the settings are the only place they are written.
pub(crate) fn share_defaults_with_language_server(cx: &mut App) {
    let fs = <dyn Fs>::global(cx);
    let mut shared: Option<Connection> = None;
    let mut share = move |cx: &mut App| {
        let connection = default_connection(cx);
        if shared.as_ref() == Some(&connection) {
            return;
        }
        shared = Some(connection.clone());
        cx.spawn({
            let fs = fs.clone();
            async move |_| write_defaults(fs, &connection).await.log_err()
        })
        .detach();
    };
    share(cx);
    cx.observe_global::<SettingsStore>(share).detach();
}

async fn write_defaults(fs: Arc<dyn Fs>, connection: &Connection) -> Result<()> {
    let folder = paths::data_dir().join("kusto");
    fs.create_dir(&folder).await?;
    fs.write(
        &folder.join(DEFAULTS_FILE),
        connection.to_defaults_file().as_bytes(),
    )
    .await
    .context("could not tell the language server the default cluster and database")
}

pub(crate) fn register(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let runs = cx.new(|_| QueryRuns::default());
    workspace.register_action({
        let runs = runs.clone();
        move |workspace, _: &RunQuery, window, cx| run_query(workspace, &runs, window, cx)
    });
    workspace.register_action(move |workspace, _: &CancelQuery, _, cx| {
        cancel_query(workspace, &runs, cx)
    });
    workspace.register_action(|_, action: &CopyClientRequestId, _, cx| {
        copy_client_request_id(&action.id, cx)
    });
    workspace.register_action(|workspace, action: &ShowResult, window, cx| {
        show_saved_result(workspace, PathBuf::from(&action.path), window, cx)
    });
}

fn copy_client_request_id(id: &str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
}

fn show_saved_result(
    _: &mut Workspace,
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.spawn_in(window, async move |workspace, cx| {
        display(&workspace, Ok(path), cx).await
    })
    .detach();
}

/// Cancels what the lens or the keyboard points at: the newest run of the query around the
/// cursor, or else the newest run of all.
fn cancel_query(workspace: &mut Workspace, runs: &Entity<QueryRuns>, cx: &mut Context<Workspace>) {
    let query = workspace
        .active_item_as::<Editor>(cx)
        .and_then(|editor| editor.update(cx, |editor, cx| query_to_run(editor, cx)))
        .map(|query| query.text);
    let cancelled = runs.update(cx, |runs, cx| runs.cancel_newest(query.as_deref(), cx));
    if let Some(run_id) = cancelled {
        workspace.dismiss_toast(&NotificationId::composite::<QueryRuns>(run_id), cx);
    }
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
    /// Shared by every run, because the client remembers tokens and token audiences.
    client: Option<Arc<KustoClient>>,
    /// Keeps this workspace's writes to the run log from overwriting each other. Another window
    /// can still lose a record to a write at the same moment, which only costs a lens.
    log_lock: Arc<futures::lock::Mutex<()>>,
}

struct ActiveRun {
    id: usize,
    request: QueryRequest,
    /// When the run started, which every record of the run carries.
    started_at: String,
    fs: Arc<dyn Fs>,
    log_lock: Arc<futures::lock::Mutex<()>>,
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
            started_at,
            fs,
            log_lock,
            task,
            ..
        } = self.active.remove(index);
        drop(task);
        cx.background_spawn(log_run(
            fs,
            log_lock,
            RunLog::of(&request, &started_at).cancelled(),
        ))
        .detach();
        if let Some(client) = client {
            cx.background_spawn(async move { client.cancel(&request).await })
                .detach_and_log_err(cx);
        }
    }

    /// Cancels the newest run of `query` if there is one, otherwise the newest run, and says
    /// which it was.
    fn cancel_newest(&mut self, query: Option<&str>, cx: &mut App) -> Option<usize> {
        let newest = |matching: Option<&str>| {
            self.active
                .iter()
                .filter(|run| matching.is_none_or(|query| run.request.query.trim() == query.trim()))
                .map(|run| run.id)
                .max()
        };
        let id = newest(query).or_else(|| newest(None))?;
        self.cancel(id, cx);
        Some(id)
    }
}

/// What to run and where, from the selection or the query around the cursor.
struct QueryToRun {
    text: String,
    connection: Connection,
}

/// The cluster and database that apply where no directive in the file says otherwise.
fn default_connection(cx: &App) -> Connection {
    let settings = KustoSettings::get_global(cx);
    Connection {
        cluster: settings.cluster.clone(),
        database: settings.database.clone(),
    }
}

fn query_to_run(editor: &mut Editor, cx: &mut Context<Editor>) -> Option<QueryToRun> {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let display_snapshot = editor.display_snapshot(cx);
    let selection = editor
        .selections
        .newest::<MultiBufferOffset>(&display_snapshot);
    let text = snapshot.text();
    let defaults = default_connection(cx);
    let (range, connection) = if selection.is_empty() {
        let resolved = resolve_query_at(&text, selection.head().0, &defaults)?;
        (resolved.range, resolved.connection)
    } else {
        let range = selection.start.0..selection.end.0;
        let connection = connection_for_selection(&text, range.clone(), &defaults);
        (range, connection)
    };
    let query = text.get(range)?.trim();
    (!query.is_empty()).then(|| QueryToRun {
        text: query.to_string(),
        connection,
    })
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

    let cluster = query.connection.cluster.as_deref().context(
        "This query has no cluster. Add // :setDefaultCluster(\"https://…\") above it, or set `kusto.cluster` in your settings.",
    )?;
    let cluster = Cluster::parse(cluster)?;
    let database = query.connection.database.clone().context(
        "This query has no database. Add // :setDefaultDb(\"…\") above it, or set `kusto.database` in your settings.",
    )?;
    let query = query.text;

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

    let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let log_lock = runs.read(cx).log_lock.clone();
    let task = cx.spawn_in(window, {
        let runs = runs.downgrade();
        let request = request.clone();
        let started_at = started_at.clone();
        let fs = fs.clone();
        let log_lock = log_lock.clone();
        async move |workspace, cx| {
            let existing = runs
                .update(cx, |runs, _| runs.client.clone())
                .log_err()
                .flatten();
            let client = match existing {
                Some(client) => client,
                None => {
                    let environment = environment.await.unwrap_or_default();
                    let client = Arc::new(KustoClient::new(
                        http_client,
                        token_provider(environment, cx),
                    ));
                    runs.update(cx, |runs, _| runs.client = Some(client.clone()))
                        .log_err();
                    client
                }
            };
            runs.update(cx, |runs, _| runs.set_client(run_id, client.clone()))
                .log_err();

            let run_log = RunLog::of(&request, &started_at);
            log_run(fs.clone(), log_lock.clone(), run_log.started()).await;
            let outcome = save_run(
                client,
                request,
                fs.clone(),
                run_uuid,
                cx.background_executor().clone(),
            )
            .await;
            log_run(
                fs,
                log_lock,
                match &outcome {
                    Ok(saved) => run_log.finished(saved),
                    Err(error) => run_log.failed(format!("{error:#}")),
                },
            )
            .await;
            publish(&workspace, &toast_id, outcome.map(|saved| saved.path), cx).await;
            runs.update(cx, |runs, _| runs.finish(run_id)).log_err();
        }
    });
    runs.update(cx, |runs, _| {
        runs.active.push(ActiveRun {
            id: run_id,
            request,
            started_at,
            fs,
            log_lock,
            client: None,
            task,
        })
    });
    Ok(())
}

/// Shows what a run produced. The run has finished, so its "running" notice goes away.
async fn publish(
    workspace: &WeakEntity<Workspace>,
    toast_id: &NotificationId,
    outcome: Result<PathBuf>,
    cx: &mut AsyncWindowContext,
) {
    workspace
        .update_in(cx, |workspace, _, cx| workspace.dismiss_toast(toast_id, cx))
        .log_err();
    display(workspace, outcome, cx).await;
}

/// Shows a saved result, or why there is none: in the Results panel, or in a tab of its own.
async fn display(
    workspace: &WeakEntity<Workspace>,
    outcome: Result<PathBuf>,
    cx: &mut AsyncWindowContext,
) {
    let in_panel = workspace
        .update_in(cx, |workspace, _, cx| {
            KustoSettings::get_global(cx).results_location == KustoResultsLocation::Panel
                && workspace.panel::<ResultsPanel>(cx).is_some()
        })
        .unwrap_or(false);

    if in_panel {
        let loaded = match outcome {
            Ok(path) => load_result_file(workspace, path, cx).await,
            Err(error) => Err(error),
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                let Some(panel) = workspace.panel::<ResultsPanel>(cx) else {
                    return;
                };
                panel.update(cx, |panel, cx| match loaded {
                    Ok(file) => panel.show_result(file, window, cx),
                    Err(error) => panel.show_error(format!("{error:#}"), cx),
                });
                workspace.open_panel::<ResultsPanel>(window, cx);
            })
            .log_err();
        return;
    }

    workspace
        .update_in(cx, |workspace, window, cx| match outcome {
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
                .detach_and_prompt_err("Could not open the query results", window, cx, |_, _, _| {
                    None
                }),
            Err(error) => workspace.show_error(error, cx),
        })
        .log_err();
}

/// Opens a saved result the way a results tab does, without making a tab.
async fn load_result_file(
    workspace: &WeakEntity<Workspace>,
    path: PathBuf,
    cx: &mut AsyncWindowContext,
) -> Result<Entity<ResultsFile>> {
    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
    let (worktree, relative_path) = project
        .update(cx, |project, cx| {
            project.find_or_create_worktree(&path, false, cx)
        })
        .await?;
    let project_path = ProjectPath {
        worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
        path: relative_path,
    };
    cx.update(|_, cx| ResultsFile::try_open(&project, &project_path, cx))?
        .with_context(|| format!("{} is not a results file", path.display()))?
        .await
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

/// A run's result, saved in the history folder.
struct SavedRun {
    path: PathBuf,
    rows: usize,
    duration_ms: u64,
}

/// Runs the query and writes its result to the history folder.
async fn save_run(
    client: Arc<KustoClient>,
    request: QueryRequest,
    fs: Arc<dyn Fs>,
    run_uuid: uuid::Uuid,
    executor: BackgroundExecutor,
) -> Result<SavedRun> {
    let (json, rows, duration_ms) = executor
        .spawn(async move {
            let result = client.execute(&request).await?;
            let started = std::time::Instant::now();
            let json = result.to_json()?;
            log::info!("kusto: serialised the result in {:?}", started.elapsed());
            Ok::<_, anyhow::Error>((
                json,
                result.total_rows(),
                result.execution_duration_ms.unwrap_or_default(),
            ))
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
    let started = std::time::Instant::now();
    fs.write(&path, json.as_bytes())
        .await
        .with_context(|| format!("could not save the result to {}", path.display()))?;
    log::info!(
        "kusto: saved {:.1} MB in {:?}",
        json.len() as f64 / 1e6,
        started.elapsed()
    );
    Ok(SavedRun {
        path,
        rows,
        duration_ms,
    })
}

/// The identity every record of one run carries.
struct RunLog {
    cid: String,
    query: String,
    cluster: String,
    database: String,
    at: String,
}

impl RunLog {
    fn of(request: &QueryRequest, started_at: &str) -> Self {
        Self {
            cid: request.client_request_id.clone(),
            query: request.query.clone(),
            cluster: request.cluster.host().to_string(),
            database: request.database.clone(),
            at: started_at.to_string(),
        }
    }

    fn started(&self) -> RunRecord {
        RunRecord::Started {
            cid: self.cid.clone(),
            query: self.query.clone(),
            cluster: self.cluster.clone(),
            database: self.database.clone(),
            at: self.at.clone(),
        }
    }

    fn finished(&self, saved: &SavedRun) -> RunRecord {
        RunRecord::Finished {
            cid: self.cid.clone(),
            query: self.query.clone(),
            cluster: self.cluster.clone(),
            database: self.database.clone(),
            at: self.at.clone(),
            duration_ms: saved.duration_ms,
            rows: saved.rows,
            path: saved.path.to_string_lossy().into_owned(),
        }
    }

    fn failed(&self, message: String) -> RunRecord {
        RunRecord::Failed {
            cid: self.cid.clone(),
            query: self.query.clone(),
            cluster: self.cluster.clone(),
            database: self.database.clone(),
            at: self.at.clone(),
            message,
        }
    }

    fn cancelled(&self) -> RunRecord {
        RunRecord::Cancelled {
            cid: self.cid.clone(),
            query: self.query.clone(),
            cluster: self.cluster.clone(),
            database: self.database.clone(),
            at: self.at.clone(),
        }
    }
}

/// Adds a record to the log the language server reads. Failing to is not worth failing a run.
async fn log_run(
    fs: Arc<dyn Fs>,
    lock: Arc<futures::lock::Mutex<()>>,
    record: RunRecord,
) {
    let _guard = lock.lock().await;
    let history = paths::data_dir().join("kusto").join("history");
    let path = history.join(RUN_LOG_FILE);
    let result = async {
        fs.create_dir(&history).await?;
        let existing = fs.load(&path).await.unwrap_or_default();
        let text = append_record(&existing, &record)?;
        fs.write(&path, text.as_bytes()).await
    }
    .await;
    result
        .context("could not record the run for the code lenses")
        .log_err();
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
    use workspace::dock::Panel as _;

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
        setup_with(cx, status, answer, QUERIES).await
    }

    /// Like [`setup`], with the text of `queries.kql` chosen by the test.
    async fn setup_with<'a>(
        cx: &'a mut TestAppContext,
        status: u16,
        answer: &'static str,
        queries: &'static str,
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
        fs.insert_tree("/root", json!({ "queries.kql": queries })).await;
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
        let panel = cx.new(ResultsPanel::new);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_panel(panel, window, cx)
        });
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

    /// Shows results in tabs of their own, as the `editor` setting does.
    fn use_editor_tabs(cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{ "kusto": { "cluster": "https://help.kusto.windows.net", "database": "Samples", "results_location": "editor" } }"#,
                        cx,
                    )
                    .expect("the user settings parse");
            });
        });
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
        use_editor_tabs(cx);

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
    async fn a_run_shows_its_result_in_the_panel_and_leaves_the_editor_active(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        assert!(!workspace.read_with(cx, |workspace, cx| workspace.bottom_dock().read(cx).is_open()));

        run(&workspace, cx);

        let viewer = panel
            .read_with(cx, |panel, _| panel.shown_viewer().cloned())
            .expect("the panel shows the result");
        assert_eq!(viewer.read_with(cx, |viewer, cx| viewer.result(cx).total_rows()), 2);
        assert_eq!(
            cx.update(|window, cx| panel.read(cx).icon_label(window, cx)),
            Some("2".to_string())
        );
        assert!(
            workspace.read_with(cx, |workspace, cx| workspace.bottom_dock().read(cx).is_open()),
            "the panel opens"
        );
        assert!(
            workspace.read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx).is_some()),
            "the query stays in front, so it can be run again"
        );
        assert_eq!(results_viewers(&workspace, cx), 0, "no tab is opened");
    }

    #[gpui::test]
    async fn the_next_run_replaces_the_result_in_the_panel(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 200, ANSWER).await;
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        let runs = cx.new(|_| QueryRuns::default());

        start(&workspace, &runs, cx);
        let first = panel
            .read_with(cx, |panel, _| panel.shown_viewer().cloned())
            .expect("a first result");
        start(&workspace, &runs, cx);
        let second = panel
            .read_with(cx, |panel, _| panel.shown_viewer().cloned())
            .expect("a second result");

        assert_eq!(bodies(&sent, "/v2/rest/query").len(), 2);
        assert_ne!(first.entity_id(), second.entity_id(), "the second run's result is shown");
        assert_eq!(results_viewers(&workspace, cx), 0);
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
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.shown_error().map(str::to_string)),
            Some("Semantic error: SEM0100".to_string())
        );
        assert_eq!(
            cx.update(|window, cx| panel.read(cx).icon_label(window, cx)),
            Some("!".to_string())
        );
        assert!(
            !workspace.read_with(cx, |workspace, _| {
                workspace.has_notification(&NotificationId::unique::<anyhow::Error>())
            }),
            "the error is in the panel, not a notification"
        );
    }

    #[gpui::test]
    async fn a_failed_run_in_editor_mode_shows_a_notification(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(
            cx,
            400,
            r#"{"error":{"message":"outer","innererror":{"@message":"Semantic error: SEM0100"}}}"#,
        )
        .await;
        use_editor_tabs(cx);

        run(&workspace, cx);

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
    async fn later_runs_reuse_what_the_first_run_learned(cx: &mut TestAppContext) {
        let (workspace, editor, sent, cx) = setup(cx, 200, ANSWER).await;
        let runs = cx.new(|_| QueryRuns::default());

        start(&workspace, &runs, cx);
        // The first run's results are now the active tab.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&editor, true, true, window, cx)
        });
        start(&workspace, &runs, cx);

        assert_eq!(bodies(&sent, "/v2/rest/query").len(), 2);
        assert_eq!(
            bodies(&sent, "/v1/rest/auth/metadata").len(),
            1,
            "the token audience is asked for once"
        );
    }

    async fn run_log(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> Vec<serde_json::Value> {
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        let path = paths::data_dir().join("kusto").join("history").join(RUN_LOG_FILE);
        fs.load(&path)
            .await
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("each record is JSON"))
            .collect()
    }

    #[gpui::test]
    async fn a_run_is_recorded_when_it_starts_and_when_it_finishes(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);

        let records = run_log(&workspace, cx).await;
        let events: Vec<&str> = records
            .iter()
            .map(|record| record["event"].as_str().expect("an event"))
            .collect();
        assert_eq!(events, ["started", "finished"]);
        assert_eq!(records[0]["query"], "StormEvents\n| take 1");
        assert_eq!(records[0]["cluster"], "help.kusto.windows.net");
        assert_eq!(records[0]["database"], "Samples");
        assert_eq!(records[0]["cid"], records[1]["cid"]);
        assert_eq!(records[0]["at"], records[1]["at"], "both name the start of the run");
        assert_eq!(records[1]["rows"], 2);
        let path = records[1]["path"].as_str().expect("the result file");
        assert!(path.ends_with(".ktt"), "{path}");
        let fs = workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone());
        assert!(fs.is_file(std::path::Path::new(path)).await, "the file is on disk");
    }

    #[gpui::test]
    async fn the_default_cluster_and_database_are_shared_with_the_language_server(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let fs = cx.update(|_, cx| <dyn Fs>::global(cx));
        let path = paths::data_dir().join("kusto").join(DEFAULTS_FILE);
        let change_settings = |settings: &'static str, cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                cx.update_global::<SettingsStore, _>(|store, cx| {
                    store
                        .set_user_settings(settings, cx)
                        .expect("the user settings parse");
                });
            });
            cx.run_until_parked();
        };
        let shared = async |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            let text = fs.load(&path).await.expect("the defaults are written");
            serde_json::from_str::<serde_json::Value>(&text).expect("the defaults are JSON")
        };

        change_settings(
            r#"{ "kusto": { "cluster": "https://help.kusto.windows.net", "database": "Samples" } }"#,
            cx,
        );
        assert_eq!(
            shared(cx).await,
            json!({ "cluster": "https://help.kusto.windows.net", "database": "Samples" })
        );

        change_settings(r#"{ "kusto": { "database": "Other" } }"#, cx);
        assert_eq!(
            shared(cx).await,
            json!({ "cluster": null, "database": "Other" }),
            "removing a setting removes it for the language server too"
        );
    }

    #[gpui::test]
    async fn a_failed_run_is_recorded_with_its_message(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(
            cx,
            400,
            r#"{"error":{"message":"outer","innererror":{"@message":"Semantic error: SEM0100"}}}"#,
        )
        .await;
        run(&workspace, cx);

        let records = run_log(&workspace, cx).await;
        assert_eq!(records[1]["event"], "failed");
        assert_eq!(records[1]["message"], "Semantic error: SEM0100");
    }

    #[gpui::test]
    async fn cancelling_from_a_lens_stops_the_run_at_the_cursor_and_records_it(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, sent, cx) = setup(cx, 0, ANSWER).await;
        let runs = cx.new(|_| QueryRuns::default());
        start(&workspace, &runs, cx);
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::composite::<QueryRuns>(1))
        }));

        workspace.update(cx, |workspace, cx| cancel_query(workspace, &runs, cx));
        cx.run_until_parked();

        assert!(runs.read_with(cx, |runs, _| runs.active.is_empty()));
        assert!(
            !workspace.read_with(cx, |workspace, _| {
                workspace.has_notification(&NotificationId::composite::<QueryRuns>(1))
            }),
            "the running notice goes away"
        );
        assert_eq!(bodies(&sent, "/v1/rest/mgmt").len(), 1, "the service is told to stop");
        let events: Vec<String> = run_log(&workspace, cx)
            .await
            .iter()
            .map(|record| record["event"].as_str().expect("an event").to_string())
            .collect();
        assert_eq!(events, ["started", "cancelled"]);
    }

    #[gpui::test]
    async fn cancelling_with_nothing_running_does_nothing(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup(cx, 200, ANSWER).await;
        let runs = cx.new(|_| QueryRuns::default());
        workspace.update(cx, |workspace, cx| cancel_query(workspace, &runs, cx));
        cx.run_until_parked();
        assert!(bodies(&sent, "/v1/rest/mgmt").is_empty());
        assert!(run_log(&workspace, cx).await.is_empty());
    }

    #[gpui::test]
    async fn cancel_prefers_the_run_of_the_query_it_points_at(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup(cx, 0, ANSWER).await;
        let runs = cx.new(|_| QueryRuns::default());
        start(&workspace, &runs, cx);
        // Move to the second query and start it too.
        editor.update_in(cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([text::Point::new(3, 0)..text::Point::new(3, 0)])
            })
        });
        start(&workspace, &runs, cx);
        assert_eq!(runs.read_with(cx, |runs, _| runs.active.len()), 2);

        // The cursor is in the second query, so that is the one that is cancelled.
        workspace.update(cx, |workspace, cx| cancel_query(workspace, &runs, cx));
        cx.run_until_parked();

        let remaining = runs.read_with(cx, |runs, _| {
            runs.active
                .iter()
                .map(|run| run.request.query.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(remaining, ["StormEvents\n| take 1".to_string()]);
    }

    #[gpui::test]
    async fn show_result_displays_a_result_from_history_in_the_panel(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        run(&workspace, cx);
        let path = run_log(&workspace, cx).await[1]["path"]
            .as_str()
            .expect("the result file")
            .to_string();
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        panel.update(cx, |panel, cx| panel.show_error("an earlier failure", cx));
        assert!(panel.read_with(cx, |panel, _| panel.shown_viewer().is_none()));

        workspace.update_in(cx, |workspace, window, cx| {
            show_saved_result(workspace, PathBuf::from(path), window, cx)
        });
        cx.run_until_parked();

        let viewer = panel
            .read_with(cx, |panel, _| panel.shown_viewer().cloned())
            .expect("the saved result replaces the error");
        assert_eq!(viewer.read_with(cx, |viewer, cx| viewer.result(cx).total_rows()), 2);
    }

    #[gpui::test]
    async fn show_result_on_a_missing_file_says_so_in_the_panel(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup(cx, 200, ANSWER).await;
        let panel = workspace
            .read_with(cx, |workspace, cx| workspace.panel::<ResultsPanel>(cx))
            .expect("the panel is added");
        workspace.update_in(cx, |workspace, window, cx| {
            show_saved_result(workspace, PathBuf::from("/gone/result.ktt"), window, cx)
        });
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.shown_error().is_some()));
    }

    #[gpui::test]
    async fn copying_a_client_request_id_puts_it_on_the_clipboard(cx: &mut TestAppContext) {
        cx.update(|cx| copy_client_request_id("ZedTracer;abc", cx));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("ZedTracer;abc".to_string())
        );
    }

    #[gpui::test]
    async fn a_directive_in_the_file_overrides_the_settings(cx: &mut TestAppContext) {
        let (workspace, _editor, sent, cx) = setup_with(
            cx,
            200,
            ANSWER,
            "// :setDefaultCluster(\"https://other.kusto.windows.net\")\n// :setDefaultDb(\"Logs\")\nT1\n| take 1",
        )
        .await;
        run(&workspace, cx);

        let body: serde_json::Value =
            serde_json::from_str(&bodies(&sent, "/v2/rest/query")[0]).expect("a JSON body");
        assert_eq!(body["db"], "Logs");
        assert!(
            body["csl"].as_str().expect("a query").ends_with("T1\n| take 1"),
            "the query keeps its comments"
        );
        let records = run_log(&workspace, cx).await;
        assert_eq!(records[0]["cluster"], "other.kusto.windows.net");
        assert_eq!(records[0]["database"], "Logs");
    }

    #[gpui::test]
    async fn changing_the_cluster_in_the_file_clears_the_database_from_the_settings(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, sent, cx) = setup_with(
            cx,
            200,
            ANSWER,
            "//:setDefaultCluster(\"https://other.kusto.windows.net\")\nT1",
        )
        .await;
        let runs = cx.new(|_| QueryRuns::default());
        let error = workspace
            .update_in(cx, |workspace, window, cx| {
                start_run(workspace, &runs, window, cx)
            })
            .expect_err("the query has no database");
        cx.run_until_parked();

        let error = error.to_string();
        assert!(error.contains("no database"), "{error}");
        assert!(error.contains("setDefaultDb"), "it says how to fix it: {error}");
        assert!(bodies(&sent, "/v2/rest/query").is_empty(), "nothing was sent");
    }

    #[gpui::test]
    async fn the_directive_above_a_query_decides_where_that_query_runs(cx: &mut TestAppContext) {
        let (workspace, editor, sent, cx) = setup_with(
            cx,
            200,
            ANSWER,
            "T1\n\n//:setDefaultDb(\"Second\")\n\nT2",
        )
        .await;
        let runs = cx.new(|_| QueryRuns::default());

        start(&workspace, &runs, cx);
        editor.update_in(cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([text::Point::new(4, 0)..text::Point::new(4, 0)])
            })
        });
        start(&workspace, &runs, cx);

        let databases: Vec<String> = bodies(&sent, "/v2/rest/query")
            .iter()
            .map(|body| {
                serde_json::from_str::<serde_json::Value>(body).expect("JSON")["db"]
                    .as_str()
                    .expect("a database")
                    .to_string()
            })
            .collect();
        assert_eq!(databases, ["Samples", "Second"]);
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
        use_editor_tabs(cx);
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
            results_location: None,
        });
        let settings = KustoSettings::from_settings(&content);
        assert_eq!(
            settings.cluster.as_deref(),
            Some("https://help.kusto.windows.net")
        );
        assert_eq!(settings.database, None);
        assert_eq!(
            settings.results_location,
            KustoResultsLocation::Panel,
            "results go to the panel unless asked otherwise"
        );
    }
}
