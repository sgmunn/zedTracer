//! Runs Kusto queries over the REST API and returns the answer as a `kusto_results::ResultSet`.
//!
//! Nothing here draws anything or knows about an editor. Where the HTTP calls go and where
//! tokens come from are both behind small interfaces, so everything can be tested without a
//! network.

mod query_text;
mod response;
mod token;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use async_compression::futures::bufread::GzipDecoder;
use futures::AsyncReadExt as _;
use futures::io::BufReader;
use http_client::{AsyncBody, HttpClient, Method, Request};
use kusto_results::ResultSet;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;

pub use query_text::query_range_at;
pub use token::{AccessToken, AzureCliTokenProvider, TokenProvider};

/// A cluster, written as `https://help.kusto.windows.net` or as just the host name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    host: String,
}

impl Cluster {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let host = match text.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
            // A token sent over plain HTTP could be read by anyone on the way.
            Some(_) => bail!("The cluster {text:?} must use https."),
            None => text,
        };
        let host = host.trim_end_matches('/');
        if host.is_empty() || host.contains(|character: char| character == '/' || character.is_whitespace()) {
            bail!(
                "The cluster {text:?} is not a cluster address such as https://help.kusto.windows.net."
            );
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn url(&self) -> String {
        format!("https://{}", self.host)
    }
}

#[derive(Clone, Debug)]
pub struct QueryRequest {
    pub cluster: Cluster,
    pub database: String,
    pub query: String,
    /// Names this run to the service and in support requests; it is also what cancels it.
    pub client_request_id: String,
}

/// Runs queries. Keep one around and reuse it: it remembers each cluster's token audience and
/// the tokens it was given, which saves a request and a sign-in call on every run after the first.
pub struct KustoClient {
    http_client: Arc<dyn HttpClient>,
    token_provider: Arc<dyn TokenProvider>,
    /// The token audience of each cluster host.
    resources: Mutex<HashMap<String, String>>,
    /// The latest token for each audience, while it has time left.
    tokens: Mutex<HashMap<String, AccessToken>>,
}

impl KustoClient {
    pub fn new(http_client: Arc<dyn HttpClient>, token_provider: Arc<dyn TokenProvider>) -> Self {
        Self {
            http_client,
            token_provider,
            resources: Mutex::default(),
            tokens: Mutex::default(),
        }
    }

    /// Runs the query and waits for the whole answer. Dropping the returned future abandons the
    /// request; call [`KustoClient::cancel`] as well to stop the work on the service.
    pub async fn execute(&self, request: &QueryRequest) -> Result<ResultSet> {
        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let timer = Instant::now();

        let body = json!({ "db": request.database, "csl": request.query });
        let body = self
            .post(
                &request.cluster,
                "/v2/rest/query",
                &request.client_request_id,
                body.to_string(),
            )
            .await?;
        let tables = response::parse_query_response(&body)?;

        Ok(ResultSet {
            query: Some(request.query.clone()),
            cluster: Some(request.cluster.host().to_string()),
            database: Some(request.database.clone()),
            execution_started_at: Some(started_at),
            execution_duration_ms: Some(
                u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX),
            ),
            client_request_id: Some(request.client_request_id.clone()),
            tables,
            ..ResultSet::default()
        })
    }

    /// Asks the service to stop the query that was started with this client request id.
    pub async fn cancel(&self, request: &QueryRequest) -> Result<()> {
        let command = format!(
            ".cancel query \"{}\"",
            request
                .client_request_id
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        );
        let body = json!({ "db": request.database, "csl": command });
        self.post(
            &request.cluster,
            "/v1/rest/mgmt",
            &format!("{};cancel", request.client_request_id),
            body.to_string(),
        )
        .await
        .context("could not cancel the query")?;
        Ok(())
    }

    /// A token for the cluster: the one from earlier while it has time left, otherwise a new one.
    async fn token(&self, cluster: &Cluster) -> Result<AccessToken> {
        let resource = self.service_resource(cluster).await;
        if let Some(token) = self.tokens.lock().get(&resource)
            && token.is_usable(SystemTime::now())
        {
            return Ok(token.clone());
        }
        let token = self.token_provider.token(&resource).await?;
        self.tokens.lock().insert(resource, token.clone());
        Ok(token)
    }

    fn forget_token(&self, cluster: &Cluster) {
        if let Some(resource) = self.resources.lock().get(cluster.host()) {
            self.tokens.lock().remove(resource);
        }
    }

    /// The service says which audience its tokens need. Every public cluster answers
    /// `https://kusto.kusto.windows.net`, but other clouds differ.
    async fn service_resource(&self, cluster: &Cluster) -> String {
        if let Some(resource) = self.resources.lock().get(cluster.host()) {
            return resource.clone();
        }
        match self.fetch_service_resource(cluster).await {
            Ok(resource) => {
                self.resources
                    .lock()
                    .insert(cluster.host().to_string(), resource.clone());
                resource
            }
            // Not remembered, so a hiccup does not stick.
            Err(error) => {
                log::warn!(
                    "could not read the token audience of {}, using the cluster address: {error:#}",
                    cluster.host()
                );
                cluster.url()
            }
        }
    }

    async fn fetch_service_resource(&self, cluster: &Cluster) -> Result<String> {
        #[derive(Deserialize)]
        struct Metadata {
            #[serde(rename = "AzureAD")]
            azure_ad: Option<AzureAd>,
        }
        #[derive(Deserialize)]
        struct AzureAd {
            #[serde(rename = "KustoServiceResourceId")]
            resource: Option<String>,
        }

        let uri = format!("{}/v1/rest/auth/metadata", cluster.url());
        let mut response = self.http_client.get(&uri, AsyncBody::empty(), true).await?;
        let mut body = Vec::new();
        response.body_mut().read_to_end(&mut body).await?;
        if !response.status().is_success() {
            return Err(response::http_error(response.status().as_u16(), &body));
        }
        let metadata: Metadata = serde_json::from_slice(&body)?;
        metadata
            .azure_ad
            .and_then(|azure_ad| azure_ad.resource)
            .filter(|resource| !resource.is_empty())
            .ok_or_else(|| anyhow!("the cluster does not name an Azure AD resource"))
    }

    /// Sends an authorized request. A token the service refuses may have been revoked or may
    /// have expired early, so one refusal gets a new token and one more try.
    async fn post(
        &self,
        cluster: &Cluster,
        path: &str,
        client_request_id: &str,
        body: String,
    ) -> Result<Vec<u8>> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let token = self.token(cluster).await?;
            let (status, response_body) = self
                .send(cluster, path, &token, client_request_id, body.clone())
                .await?;
            if status == 401 && attempts == 1 {
                self.forget_token(cluster);
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(response::http_error(status, &response_body));
            }
            return Ok(response_body);
        }
    }

    async fn send(
        &self,
        cluster: &Cluster,
        path: &str,
        token: &AccessToken,
        client_request_id: &str,
        body: String,
    ) -> Result<(u16, Vec<u8>)> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{}{path}", cluster.url()))
            .header("Authorization", format!("Bearer {}", token.secret()))
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Accept", "application/json")
            // The HTTP client does not decompress by itself, so this has to be handled below.
            .header("Accept-Encoding", "gzip")
            .header("x-ms-client-request-id", client_request_id)
            .body(AsyncBody::from(body))
            .context("could not build the request")?;
        let mut response = self
            .http_client
            .send(request)
            .await
            .with_context(|| format!("could not reach {}", cluster.host()))?;
        let is_gzip = response
            .headers()
            .get("Content-Encoding")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("gzip"));

        let mut body = Vec::new();
        if is_gzip {
            GzipDecoder::new(BufReader::new(response.body_mut()))
                .read_to_end(&mut body)
                .await
        } else {
            response.body_mut().read_to_end(&mut body).await
        }
        .context("could not read the response")?;
        Ok((response.status().as_u16(), body))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use futures::executor::block_on;
    use futures::future::BoxFuture;
    use http_client::FakeHttpClient;
    use http_client::Response;
    use pretty_assertions::assert_eq;

    use super::*;

    /// Hands out tokens named `secret-1`, `secret-2`, and so on, with a lifetime only if asked to.
    #[derive(Default)]
    struct FixedToken {
        resources: Mutex<Vec<String>>,
        expiring: Mutex<bool>,
    }

    impl TokenProvider for Arc<FixedToken> {
        fn token(&self, resource: &str) -> BoxFuture<'static, Result<AccessToken>> {
            let mut resources = self.resources.lock().expect("test lock");
            resources.push(resource.to_string());
            let secret = format!("secret-{}", resources.len());
            let token = if *self.expiring.lock().expect("test lock") {
                AccessToken::expiring_at(secret, SystemTime::now() + Duration::from_secs(3600))
            } else {
                AccessToken::new(secret)
            };
            Box::pin(async { Ok(token) })
        }
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    struct Reply {
        status: u16,
        body: Vec<u8>,
        gzip: bool,
    }

    fn client(
        answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static,
    ) -> (KustoClient, Seen, Arc<FixedToken>) {
        client_with(move |path, _| {
            let (status, body) = answer(path);
            Reply {
                status,
                body: body.into_bytes(),
                gzip: false,
            }
        })
    }

    /// `reply` is given the path and how many requests that path has had before this one.
    fn client_with(
        reply: impl Fn(&str, usize) -> Reply + Send + Sync + 'static,
    ) -> (KustoClient, Seen, Arc<FixedToken>) {
        let seen: Seen = Arc::default();
        let recorded = seen.clone();
        let http = FakeHttpClient::create(move |request| {
            let recorded = recorded.clone();
            let path = request.uri().path().to_string();
            let earlier = recorded
                .lock()
                .expect("test lock")
                .iter()
                .filter(|(seen_path, ..)| *seen_path == path)
                .count();
            let reply = reply(&path, earlier);
            async move {
                let header = |name: &str| {
                    request
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let authorization = header("Authorization");
                let request_id = header("x-ms-client-request-id");
                let mut sent = String::new();
                request.into_body().read_to_string(&mut sent).await?;
                recorded
                    .lock()
                    .expect("test lock")
                    .push((path, authorization, request_id, sent));

                let mut response = Response::builder().status(reply.status);
                let body = if reply.gzip {
                    response = response.header("Content-Encoding", "gzip");
                    let mut compressed = Vec::new();
                    async_compression::futures::bufread::GzipEncoder::new(
                        futures::io::Cursor::new(reply.body),
                    )
                    .read_to_end(&mut compressed)
                    .await?;
                    compressed
                } else {
                    reply.body
                };
                Ok(response.body(AsyncBody::from(body))?)
            }
        });
        let tokens = Arc::new(FixedToken::default());
        (
            KustoClient::new(http, Arc::new(tokens.clone())),
            seen,
            tokens,
        )
    }

    fn request() -> QueryRequest {
        QueryRequest {
            cluster: Cluster::parse("https://Help.Kusto.Windows.net/").expect("cluster"),
            database: "Samples".into(),
            query: "print a=1".into(),
            client_request_id: "ZedTracer;1".into(),
        }
    }

    const METADATA: &str = r#"{"AzureAD":{"KustoServiceResourceId":"https://kusto.kusto.windows.net"}}"#;
    const ANSWER: &str = r#"[{"FrameType":"DataTable","TableKind":"PrimaryResult","TableName":"PrimaryResult",
        "Columns":[{"ColumnName":"a","ColumnType":"long"}],"Rows":[[1]]},
        {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}]"#;

    fn answer(path: &str) -> (u16, String) {
        match path {
            "/v1/rest/auth/metadata" => (200, METADATA.into()),
            _ => (200, ANSWER.into()),
        }
    }

    #[test]
    fn runs_a_query_and_records_where_and_when() {
        let (client, seen, tokens) = client(answer);
        let result = block_on(client.execute(&request())).expect("result");

        assert_eq!(result.tables[0].rows[0][0].display_text(), "1");
        assert_eq!(result.cluster.as_deref(), Some("help.kusto.windows.net"));
        assert_eq!(result.database.as_deref(), Some("Samples"));
        assert_eq!(result.query.as_deref(), Some("print a=1"));
        assert_eq!(result.client_request_id.as_deref(), Some("ZedTracer;1"));
        assert!(result.execution_started_at.is_some());

        assert_eq!(
            *tokens.resources.lock().expect("test lock"),
            ["https://kusto.kusto.windows.net"]
        );
        let seen = seen.lock().expect("test lock");
        let (path, authorization, request_id, body) = &seen[1];
        assert_eq!(path, "/v2/rest/query");
        assert_eq!(authorization, "Bearer secret-1");
        assert_eq!(request_id, "ZedTracer;1");
        assert_eq!(body, r#"{"db":"Samples","csl":"print a=1"}"#);
    }

    #[test]
    fn falls_back_to_the_cluster_address_when_the_audience_is_unknown() {
        let (client, _, tokens) = client(|path| match path {
            "/v1/rest/auth/metadata" => (404, String::new()),
            _ => (200, ANSWER.into()),
        });
        block_on(client.execute(&request())).expect("result");
        assert_eq!(
            *tokens.resources.lock().expect("test lock"),
            ["https://help.kusto.windows.net"]
        );
    }

    #[test]
    fn reports_the_message_of_a_failed_query() {
        let (client, _, _) = client(|path| match path {
            "/v1/rest/auth/metadata" => (200, METADATA.into()),
            _ => (
                400,
                r#"{"error":{"message":"outer","innererror":{"@message":"Semantic error: SEM0100"}}}"#.into(),
            ),
        });
        let error = block_on(client.execute(&request())).expect_err("error");
        assert_eq!(error.to_string(), "Semantic error: SEM0100");
    }

    #[test]
    fn cancel_asks_the_service_to_stop_the_run() {
        let (client, seen, _) = client(|path| match path {
            "/v1/rest/auth/metadata" => (200, METADATA.into()),
            _ => (200, "{}".into()),
        });
        block_on(client.cancel(&request())).expect("cancelled");
        let seen = seen.lock().expect("test lock");
        let (path, _, request_id, body) = &seen[1];
        assert_eq!(path, "/v1/rest/mgmt");
        assert_eq!(request_id, "ZedTracer;1;cancel");
        assert_eq!(
            body,
            r#"{"db":"Samples","csl":".cancel query \"ZedTracer;1\""}"#
        );
    }

    fn count(seen: &Seen, path: &str) -> usize {
        seen.lock()
            .expect("test lock")
            .iter()
            .filter(|(seen_path, ..)| seen_path == path)
            .count()
    }

    #[test]
    fn a_second_run_reuses_the_audience_and_a_token_that_has_time_left() {
        let (client, seen, tokens) = client(answer);
        *tokens.expiring.lock().expect("test lock") = true;

        block_on(client.execute(&request())).expect("first run");
        block_on(client.execute(&request())).expect("second run");

        assert_eq!(count(&seen, "/v1/rest/auth/metadata"), 1);
        assert_eq!(tokens.resources.lock().expect("test lock").len(), 1);
        assert_eq!(count(&seen, "/v2/rest/query"), 2);
        let seen = seen.lock().expect("test lock");
        assert!(seen.iter().filter(|(path, ..)| path == "/v2/rest/query").all(
            |(_, authorization, ..)| authorization == "Bearer secret-1"
        ));
    }

    #[test]
    fn a_token_without_a_known_end_is_asked_for_again_but_the_audience_is_not() {
        let (client, seen, tokens) = client(answer);

        block_on(client.execute(&request())).expect("first run");
        block_on(client.execute(&request())).expect("second run");

        assert_eq!(count(&seen, "/v1/rest/auth/metadata"), 1);
        assert_eq!(tokens.resources.lock().expect("test lock").len(), 2);
    }

    #[test]
    fn a_failed_audience_lookup_is_tried_again_on_the_next_run() {
        let (client, seen, _) = client(|path| match path {
            "/v1/rest/auth/metadata" => (503, String::new()),
            _ => (200, ANSWER.into()),
        });
        block_on(client.execute(&request())).expect("first run");
        block_on(client.execute(&request())).expect("second run");
        assert_eq!(count(&seen, "/v1/rest/auth/metadata"), 2);
    }

    #[test]
    fn a_refused_token_is_replaced_and_the_request_tried_once_more() {
        let (client, seen, tokens) = client_with(|path, earlier| match path {
            "/v1/rest/auth/metadata" => Reply {
                status: 200,
                body: METADATA.as_bytes().to_vec(),
                gzip: false,
            },
            _ if earlier == 0 => Reply {
                status: 401,
                body: Vec::new(),
                gzip: false,
            },
            _ => Reply {
                status: 200,
                body: ANSWER.as_bytes().to_vec(),
                gzip: false,
            },
        });
        *tokens.expiring.lock().expect("test lock") = true;

        block_on(client.execute(&request())).expect("the retry succeeds");

        let seen = seen.lock().expect("test lock");
        let used: Vec<&str> = seen
            .iter()
            .filter(|(path, ..)| path == "/v2/rest/query")
            .map(|(_, authorization, ..)| authorization.as_str())
            .collect();
        assert_eq!(used, ["Bearer secret-1", "Bearer secret-2"]);
    }

    #[test]
    fn a_token_that_is_refused_twice_is_reported() {
        let (client, seen, _) = client(|path| match path {
            "/v1/rest/auth/metadata" => (200, METADATA.into()),
            _ => (401, String::new()),
        });
        let error = block_on(client.execute(&request())).expect_err("refused");
        assert!(error.to_string().contains("rejected the access token"));
        assert_eq!(count(&seen, "/v2/rest/query"), 2);
    }

    #[test]
    fn a_compressed_response_is_decoded() {
        let (client, _, _) = client_with(|path, _| Reply {
            status: 200,
            body: if path == "/v1/rest/auth/metadata" {
                METADATA.as_bytes().to_vec()
            } else {
                ANSWER.as_bytes().to_vec()
            },
            gzip: path != "/v1/rest/auth/metadata",
        });
        let result = block_on(client.execute(&request())).expect("result");
        assert_eq!(result.tables[0].rows[0][0].display_text(), "1");
    }

    #[test]
    fn cluster_accepts_a_url_or_a_host_and_refuses_plain_http() {
        let expected = Cluster::parse("help.kusto.windows.net").expect("host");
        assert_eq!(Cluster::parse(" https://HELP.kusto.windows.net/ ").expect("url"), expected);
        assert_eq!(expected.url(), "https://help.kusto.windows.net");
        assert!(Cluster::parse("http://help.kusto.windows.net").is_err());
        assert!(Cluster::parse("").is_err());
        assert!(Cluster::parse("https://help.kusto.windows.net/Samples").is_err());
        assert!(Cluster::parse("not a cluster").is_err());
    }
}
