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

#[test]
#[ignore = "needs a network and an Azure CLI sign-in"]
fn timings() -> Result<()> {
    use std::time::Instant;

    let client = client();
    let time = |label: &str, started: Instant| {
        println!("{label:<40} {:>7.0} ms", started.elapsed().as_secs_f64() * 1000.0)
    };

    let started = Instant::now();
    block_on(client.execute(&request("print 1")?))?;
    time("1st run (audience + az + round trip)", started);
    let started = Instant::now();
    block_on(client.execute(&request("print 1")?))?;
    time("2nd run (round trip only)", started);
    let started = Instant::now();
    block_on(client.execute(&request("print 1")?))?;
    time("3rd run (round trip only)", started);

    let query = "range x from 1 to 20000 step 1 \
        | extend Message = strcat('message-', tostring(x), ' some words to make a realistic line of text'), \
                 Payload = bag_pack('id', x, 'name', strcat('n', tostring(x)), 'nested', bag_pack('a', 1, 'b', dynamic([1,2,3])))";
    let started = Instant::now();
    let result = block_on(client.execute(&request(query)?))?;
    time("20000-row query (fetch + parse)", started);

    let started = Instant::now();
    let json = result.to_json()?;
    time(&format!("to_json ({} MB)", json.len() / 1_000_000), started);
    let started = Instant::now();
    kusto_results::ResultSet::from_json(&json)?;
    time("from_json (what opening the file does)", started);
    Ok(())
}
