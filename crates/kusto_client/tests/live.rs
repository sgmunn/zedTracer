//! Runs a real query against the public Kusto help cluster with the signed-in Azure CLI.
//!
//! Ignored by default because it needs a network and `az login`:
//! `cargo test -p kusto_client --test live -- --ignored --nocapture`

use std::sync::Arc;

use anyhow::Result;
use futures::executor::block_on;
use kusto_client::{AzureCliTokenProvider, Cluster, KustoClient, QueryRequest};
use reqwest_client::ReqwestClient;

fn client() -> KustoClient {
    KustoClient::new(
        Arc::new(ReqwestClient::new()),
        Arc::new(AzureCliTokenProvider::new(std::env::vars())),
    )
}

fn request(query: &str) -> Result<QueryRequest> {
    Ok(QueryRequest {
        cluster: Cluster::parse("https://help.kusto.windows.net")?,
        database: "Samples".into(),
        query: query.into(),
        client_request_id: "ZedTracer;live-test".into(),
    })
}

#[test]
#[ignore = "needs a network and an Azure CLI sign-in"]
fn runs_a_query_on_the_help_cluster() -> Result<()> {
    let result = block_on(client().execute(&request(
        "print a=1, b='x', c=now(), d=dynamic({'k':[1,2]}); StormEvents | take 3 | project State, StartTime",
    )?))?;
    for table in &result.tables {
        println!("{}: {} rows", table.name, table.rows.len());
    }
    assert_eq!(result.tables.len(), 2);
    assert_eq!(result.tables[0].rows.len(), 1);
    assert_eq!(result.tables[1].rows.len(), 3);
    Ok(())
}

#[test]
#[ignore = "needs a network and an Azure CLI sign-in"]
fn reports_why_a_query_failed() -> Result<()> {
    let error = block_on(client().execute(&request("NoSuchTable | take 1")?))
        .expect_err("the table does not exist");
    println!("{error}");
    assert!(error.to_string().contains("NoSuchTable"));
    Ok(())
}
