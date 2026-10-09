//! Asking the Kusto language server to qualify the names of a query, for the copy of a query and
//! for the query a result file keeps. The server has the schema, which says what is a table of the
//! database and what is a column, and the editor has no parser to tell them apart.

use std::sync::Arc;
use std::time::Duration;

use gpui::App;
use gpui_util::ResultExt as _;
use kusto_client::{Connection, Qualified};
use lsp::LanguageServer;
use project::Project;
use serde_json::json;
use util::ConnectionResult;

const SERVER_NAME: &str = "kusto-lsp";
const QUALIFY_COMMAND: &str = "kusto.qualifyQuery";

/// A query is not worth waiting for: a run or a copy goes on without the server's answer.
const QUALIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// A running Kusto language server of the project. Any one will do, because the question carries
/// the text and the connection and the schema is the same in each.
pub(crate) fn language_server(project: &Project, cx: &App) -> Option<Arc<LanguageServer>> {
    let lsp_store = project.lsp_store();
    project
        .language_server_statuses(cx)
        .filter(|(_, status)| status.name.0.as_ref() == SERVER_NAME)
        .find_map(|(server_id, _)| lsp_store.read(cx).language_server_for_id(server_id))
}

/// The query with its names qualified, or `None` when there is no server, the connection is not
/// whole, or the server did not answer.
pub(crate) async fn qualify(
    server: Option<Arc<LanguageServer>>,
    query: &str,
    connection: &Connection,
) -> Option<Qualified> {
    let server = server?;
    let (cluster, database) = connection
        .cluster
        .as_ref()
        .zip(connection.database.as_ref())?;
    let response = server
        .request::<lsp::request::ExecuteCommand>(
            lsp::ExecuteCommandParams {
                command: QUALIFY_COMMAND.to_string(),
                arguments: vec![json!({ "text": query, "cluster": cluster, "database": database })],
                ..Default::default()
            },
            QUALIFY_TIMEOUT,
        )
        .await;
    match response {
        ConnectionResult::Result(Ok(Some(answer))) => serde_json::from_value(answer).log_err(),
        other => {
            log::warn!("kusto: the language server did not qualify the query: {other:?}");
            None
        }
    }
}
