//! Runs Kusto queries over the REST API and returns the answer as a `kusto_results::ResultSet`.
//!
//! Nothing here draws anything or knows about an editor. Where the HTTP calls go and where
//! tokens come from are both behind small interfaces, so everything can be tested without a
//! network.

mod query_text;
mod response;
mod token;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Method, Request};
use kusto_results::ResultSet;
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

pub struct KustoClient {
    http_client: Arc<dyn HttpClient>,
    token_provider: Arc<dyn TokenProvider>,
}

impl KustoClient {
    pub fn new(http_client: Arc<dyn HttpClient>, token_provider: Arc<dyn TokenProvider>) -> Self {
        Self {
            http_client,
            token_provider,
        }
    }

    /// Runs the query and waits for the whole answer. Dropping the returned future abandons the
    /// request; call [`KustoClient::cancel`] as well to stop the work on the service.
    pub async fn execute(&self, request: &QueryRequest) -> Result<ResultSet> {
        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let timer = Instant::now();

        let token = self.token(&request.cluster).await?;
        let body = json!({ "db": request.database, "csl": request.query });
        let body = self
            .post(
                &request.cluster,
                "/v2/rest/query",
                &token,
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
        let token = self.token(&request.cluster).await?;
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
            &token,
            &format!("{};cancel", request.client_request_id),
            body.to_string(),
        )
        .await
        .context("could not cancel the query")?;
        Ok(())
    }

    /// The service says which audience its tokens need. Every public cluster answers
    /// `https://kusto.kusto.windows.net`, but other clouds differ.
    async fn token(&self, cluster: &Cluster) -> Result<AccessToken> {
        let resource = self.service_resource(cluster).await;
        self.token_provider.token(&resource).await
    }

    async fn service_resource(&self, cluster: &Cluster) -> String {
        match self.fetch_service_resource(cluster).await {
            Ok(resource) => resource,
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

    async fn post(
        &self,
        cluster: &Cluster,
        path: &str,
        token: &AccessToken,
        client_request_id: &str,
        body: String,
    ) -> Result<Vec<u8>> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{}{path}", cluster.url()))
            .header("Authorization", format!("Bearer {}", token.secret()))
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Accept", "application/json")
            .header("x-ms-client-request-id", client_request_id)
            .body(AsyncBody::from(body))
            .context("could not build the request")?;
        let mut response = self
            .http_client
            .send(request)
            .await
            .with_context(|| format!("could not reach {}", cluster.host()))?;
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("could not read the response")?;
        if !response.status().is_success() {
            return Err(response::http_error(response.status().as_u16(), &body));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::executor::block_on;
    use futures::future::BoxFuture;
    use http_client::FakeHttpClient;
    use http_client::Response;
    use pretty_assertions::assert_eq;

    use super::*;

    struct FixedToken {
        resources: Mutex<Vec<String>>,
    }

    impl TokenProvider for Arc<FixedToken> {
        fn token(&self, resource: &str) -> BoxFuture<'static, Result<AccessToken>> {
            self.resources
                .lock()
                .expect("test lock")
                .push(resource.to_string());
            Box::pin(async { Ok(AccessToken::new("secret")) })
        }
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    fn client(answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> (KustoClient, Seen, Arc<FixedToken>) {
        let seen: Seen = Arc::default();
        let recorded = seen.clone();
        let http = FakeHttpClient::create(move |request| {
            let recorded = recorded.clone();
            let (status, body) = answer(request.uri().path());
            async move {
                let path = request.uri().path().to_string();
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
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body))?)
            }
        });
        let tokens = Arc::new(FixedToken {
            resources: Mutex::default(),
        });
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
        assert_eq!(authorization, "Bearer secret");
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
