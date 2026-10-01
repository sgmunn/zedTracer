use std::collections::HashMap;
use std::fmt;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::future::BoxFuture;
use serde::Deserialize;

/// A bearer token. Its `Debug` output hides the secret so a token never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(..)")
    }
}

/// Where access tokens for a Kusto service come from.
pub trait TokenProvider: Send + Sync + 'static {
    /// A token whose audience is `resource`, for example `https://kusto.kusto.windows.net`.
    fn token(&self, resource: &str) -> BoxFuture<'static, Result<AccessToken>>;
}

/// Asks the Azure CLI for a token, so the user's `az login` session is the sign-in.
pub struct AzureCliTokenProvider {
    environment: HashMap<String, String>,
}

impl AzureCliTokenProvider {
    /// `environment` is the shell environment the CLI runs in. A desktop app does not inherit
    /// the `PATH` of the user's shell, which is where `az` is usually installed.
    pub fn new(environment: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            environment: environment.into_iter().collect(),
        }
    }
}

impl TokenProvider for AzureCliTokenProvider {
    fn token(&self, resource: &str) -> BoxFuture<'static, Result<AccessToken>> {
        let environment = self.environment.clone();
        let resource = resource.to_string();
        Box::pin(async move {
            let search_path = environment.get("PATH").map(String::as_str);
            let program = match search_path {
                Some(search_path) => which::which_in("az", Some(search_path), "."),
                None => which::which("az"),
            }
            .map_err(|_| {
                anyhow!("The Azure CLI (az) was not found. Install it, then run `az login`.")
            })?;

            let output = util::command::new_command(program)
                .args([
                    "account",
                    "get-access-token",
                    "--resource",
                    resource.as_str(),
                    "--output",
                    "json",
                ])
                .envs(&environment)
                .stdin(util::command::Stdio::null())
                .output()
                .await
                .context("could not run the Azure CLI")?;
            if !output.status.success() {
                let message = String::from_utf8_lossy(&output.stderr);
                bail!("The Azure CLI could not get a token: {}", message.trim());
            }
            parse_az_output(&output.stdout)
        })
    }
}

fn parse_az_output(stdout: &[u8]) -> Result<AccessToken> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AzureToken {
        access_token: String,
    }

    let token: AzureToken = serde_json::from_slice(stdout)
        .context("the Azure CLI did not return a token in the expected form")?;
    if token.access_token.is_empty() {
        bail!("the Azure CLI returned an empty token");
    }
    Ok(AccessToken::new(token.access_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_token_from_az_output() {
        let token = parse_az_output(
            br#"{"accessToken":"secret","expiresOn":"2026-10-01 12:00:00.000000","tokenType":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(token.secret(), "secret");
    }

    #[test]
    fn rejects_output_without_a_token() {
        assert!(parse_az_output(b"{}").is_err());
        assert!(parse_az_output(br#"{"accessToken":""}"#).is_err());
        assert!(parse_az_output(b"not json").is_err());
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let token = AccessToken::new("secret");
        assert!(!format!("{token:?}").contains("secret"));
    }
}
