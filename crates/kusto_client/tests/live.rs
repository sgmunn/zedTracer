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
    // The app builds its client with a user agent; `KUSTO_TIMING_APP_CLIENT=1` does the same here.
    let http: Arc<dyn http_client::HttpClient> = if std::env::var("KUSTO_TIMING_APP_CLIENT").is_ok()
    {
        Arc::new(
            ReqwestClient::proxy_and_user_agent(None, "Zed/0.0.0 (macos; aarch64)")
                .expect("client"),
        )
    } else {
        Arc::new(ReqwestClient::new())
    };
    KustoClient::new(http, Arc::new(AzureCliTokenProvider::new(std::env::vars())))
}

fn request(query: &str) -> Result<QueryRequest> {
    Ok(QueryRequest {
        cluster: Cluster::parse("https://help.kusto.windows.net")?,
        database: "Samples".into(),
        query: query.into(),
        client_request_id: "ZedTracer;live-test".into(),
        parameters: Default::default(),
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
fn passes_values_for_declared_query_parameters() -> Result<()> {
    let mut request = request(
        "declare query_parameters(raid:string, count:long, since:datetime); print raid, count, since",
    )?;
    request.parameters = [
        ("raid", "abc\"; drop"),
        ("count", "5"),
        ("since", "2024-01-02T03:04:05Z"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();
    let result = block_on(client().execute(&request))?;
    let row = &result.tables[0].rows[0];
    let shown: Vec<String> = row
        .iter()
        .map(|value| value.display_text().into_owned())
        .collect();
    println!("{shown:?}");
    assert_eq!(shown[0], "abc\"; drop");
    assert_eq!(shown[1], "5");
    assert!(shown[2].contains("2024-01-02"), "{}", shown[2]);
    Ok(())
}

#[test]
#[ignore = "needs a network and an Azure CLI sign-in"]
fn reports_a_declared_parameter_that_was_given_no_value() -> Result<()> {
    let error = block_on(client().execute(&request(
        "declare query_parameters(raid:string); print raid",
    )?))
    .expect_err("raid has no value");
    println!("{error}");
    assert!(error.to_string().contains("raid"));
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
        println!(
            "{label:<40} {:>7.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        )
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

/// Times a query read from the file named by `KUSTO_TIMING_QUERY`, on the cluster and database
/// from `KUSTO_TIMING_CLUSTER` and `KUSTO_TIMING_DATABASE`. Prints sizes, never values.
#[test]
#[ignore = "needs a network, an Azure CLI sign-in and environment variables"]
fn time_a_query_from_a_file() -> Result<()> {
    use std::time::Instant;

    let query = std::fs::read_to_string(std::env::var("KUSTO_TIMING_QUERY")?)?;
    let request = QueryRequest {
        cluster: Cluster::parse(&std::env::var("KUSTO_TIMING_CLUSTER")?)?,
        database: std::env::var("KUSTO_TIMING_DATABASE")?,
        query,
        client_request_id: "ZedTracer;timing".into(),
        parameters: Default::default(),
    };
    let client = client();
    block_on(client.execute(&self::request("print 1")?)).ok();
    for run in 1..=3 {
        let started = Instant::now();
        let result = block_on(client.execute(&request))?;
        let fetched = started.elapsed();
        let started = Instant::now();
        let json = result.to_json()?;
        let serialised = started.elapsed();
        if let Ok(path) = std::env::var("KUSTO_TIMING_SAVE") {
            std::fs::write(path, &json)?;
        }
        let started = Instant::now();
        kusto_results::ResultSet::from_json(&json)?;
        let parsed = started.elapsed();
        println!(
            "run {run}: execute {:>6.0} ms ({} tables, {} rows, server says {} ms) | to_json {:>4.0} ms ({:.1} MB) | from_json {:>4.0} ms",
            fetched.as_secs_f64() * 1000.0,
            result.tables.len(),
            result.total_rows(),
            result.execution_duration_ms.unwrap_or_default(),
            serialised.as_secs_f64() * 1000.0,
            json.len() as f64 / 1e6,
            parsed.as_secs_f64() * 1000.0,
        );
    }
    Ok(())
}

/// Which way of building the HTTP client makes a download slow? Uses a result of about 1 MB.
#[test]
#[ignore = "needs a network and an Azure CLI sign-in"]
fn download_speed_by_client_construction() -> Result<()> {
    use std::time::Instant;

    let query = "range x from 1 to 2000 step 1 \
        | extend Message = strcat('message-', tostring(x), ' some words to make a realistic line of text and a bit more text to pad it out'), \
                 Payload = bag_pack('id', x, 'name', strcat('n', tostring(x)), 'nested', bag_pack('a', 1, 'b', dynamic([1,2,3])))";
    const AGENT: &str = "Zed/0.0.0 (macos; aarch64)";
    let variants: Vec<(&str, Arc<dyn http_client::HttpClient>)> = vec![
        ("plain (ReqwestClient::new)", Arc::new(ReqwestClient::new())),
        (
            "user agent only (ReqwestClient::user_agent)",
            Arc::new(ReqwestClient::user_agent(AGENT)?),
        ),
        (
            "user agent + preconfigured TLS (what the app uses)",
            Arc::new(ReqwestClient::proxy_and_user_agent(None, AGENT)?),
        ),
    ];
    for (label, http) in variants {
        let client = KustoClient::new(http, Arc::new(AzureCliTokenProvider::new(std::env::vars())));
        block_on(client.execute(&request("print 1")?))?;
        let started = Instant::now();
        let result = block_on(client.execute(&request(query)?))?;
        println!(
            "{label:<55} {:>7.0} ms for {} rows",
            started.elapsed().as_secs_f64() * 1000.0,
            result.total_rows()
        );
    }
    Ok(())
}

#[test]
#[ignore = "needs a network"]
fn http_version_by_client_construction() -> Result<()> {
    const AGENT: &str = "Zed/0.0.0 (macos; aarch64)";
    let variants: Vec<(&str, Arc<dyn http_client::HttpClient>)> = vec![
        ("plain", Arc::new(ReqwestClient::new())),
        (
            "user agent only",
            Arc::new(ReqwestClient::user_agent(AGENT)?),
        ),
        (
            "app (agent + preconfigured TLS)",
            Arc::new(ReqwestClient::proxy_and_user_agent(None, AGENT)?),
        ),
    ];
    for (label, http) in variants {
        let response = block_on(http.get(
            "https://trd-8uhsupt16c3grpr2jy.z9.kusto.fabric.microsoft.com/v1/rest/auth/metadata",
            http_client::AsyncBody::empty(),
            true,
        ))?;
        println!("{label:<35} {:?}", response.version());
    }
    Ok(())
}
