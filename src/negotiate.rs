//! HTTP negotiate step before opening the WebSocket transport.

use std::time::Duration;

use crate::error::Error;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

const DEFAULT_NEGOTIATE_VERSION: u32 = 1;
const MAX_NEGOTIATE_REDIRECTS: u32 = 100;

/// Response from the SignalR `/negotiate` endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NegotiateResponse {
    pub url: Option<String>,
    pub access_token: Option<String>,
    pub error: Option<String>,
    pub connection_id: Option<String>,
    pub connection_token: Option<String>,
    pub negotiate_version: Option<u32>,
    pub use_stateful_reconnect: Option<bool>,
}

impl NegotiateResponse {
    /// Returns the token used in the WebSocket URL query parameter `id`.
    pub fn websocket_token(&self) -> Result<&str, Error> {
        if let Some(error) = &self.error {
            return Err(Error::Negotiate(error.clone()));
        }

        self.connection_token
            .as_deref()
            .or(self.connection_id.as_deref())
            .ok_or_else(|| Error::Negotiate("missing connectionToken and connectionId".into()))
    }

    fn normalize(mut self) -> Self {
        if self.negotiate_version.unwrap_or(0) < 1 {
            self.connection_token = self.connection_token.or_else(|| self.connection_id.clone());
        }
        self
    }
}

/// Result of a completed negotiate sequence (including redirects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiateResult {
    pub response: NegotiateResponse,
    pub access_token: Option<String>,
}

/// Options for the negotiate request.
#[derive(Debug, Clone)]
pub struct NegotiateOptions {
    pub negotiate_version: u32,
    pub use_stateful_reconnect: bool,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
}

impl Default for NegotiateOptions {
    fn default() -> Self {
        Self {
            negotiate_version: DEFAULT_NEGOTIATE_VERSION,
            use_stateful_reconnect: false,
            headers: Vec::new(),
            timeout: Duration::from_secs(100),
        }
    }
}

impl NegotiateOptions {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Performs negotiate, following server redirects until a final response is received.
pub async fn negotiate(
    hub_url: &str,
    options: &NegotiateOptions,
    access_token: Option<&str>,
) -> Result<NegotiateResult, Error> {
    let client = Client::builder().timeout(options.timeout).build()?;
    negotiate_with_client(hub_url, options, &client, access_token).await
}

/// Performs negotiate using a caller-provided HTTP client.
pub async fn negotiate_with_client(
    hub_url: &str,
    options: &NegotiateOptions,
    client: &Client,
    mut access_token: Option<&str>,
) -> Result<NegotiateResult, Error> {
    let mut current_url = hub_url.to_string();
    let mut redirects = 0;
    let mut stored_access_token = access_token.take().map(str::to_owned);

    loop {
        let response = negotiate_once(
            &current_url,
            options,
            client,
            stored_access_token.as_deref(),
        )
        .await?;

        if let Some(error) = &response.error {
            return Err(Error::Negotiate(error.clone()));
        }

        if (response.use_stateful_reconnect.unwrap_or(false)) && !options.use_stateful_reconnect {
            return Err(Error::Negotiate(
                "client did not negotiate stateful reconnect but the server did".into(),
            ));
        }

        if let Some(token) = &response.access_token {
            stored_access_token = Some(token.clone());
        }

        let Some(redirect_url) = response.url.clone() else {
            return Ok(NegotiateResult {
                response,
                access_token: stored_access_token,
            });
        };

        if redirects >= MAX_NEGOTIATE_REDIRECTS {
            return Err(Error::Negotiate(
                "negotiate redirection limit exceeded".into(),
            ));
        }

        current_url = redirect_url;
        redirects += 1;
    }
}

async fn negotiate_once(
    hub_url: &str,
    options: &NegotiateOptions,
    client: &Client,
    access_token: Option<&str>,
) -> Result<NegotiateResponse, Error> {
    let negotiate_url = resolve_negotiate_url(hub_url, options)?;
    let mut request = client
        .post(negotiate_url)
        .header("Accept", "application/json");

    for (name, value) in &options.headers {
        request = request.header(name, value);
    }

    if let Some(token) = access_token.filter(|token| !token.is_empty()) {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request.send().await?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Negotiate(format!(
            "unexpected status code returned from negotiate: {}",
            response.status()
        )));
    }

    Ok(response.json::<NegotiateResponse>().await?.normalize())
}

/// Builds the WebSocket URL used after negotiate completes.
pub fn build_websocket_url(
    base_url: &str,
    connection_token: &str,
    access_token: Option<&str>,
) -> Result<String, Error> {
    let mut parsed = Url::parse(base_url)?;
    parsed.set_fragment(None);

    let mut query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| key != "negotiateVersion")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    query_pairs.push(("id".into(), connection_token.to_string()));

    parsed.set_query(None);
    {
        let mut query = parsed.query_pairs_mut();
        for (key, value) in query_pairs {
            query.append_pair(&key, &value);
        }
    }

    let mut url = parsed.to_string();
    url = url.replacen("https://", "wss://", 1);
    url = url.replacen("http://", "ws://", 1);

    crate::transport::append_access_token(&url, access_token)
}

fn resolve_negotiate_url(hub_url: &str, options: &NegotiateOptions) -> Result<String, Error> {
    let mut negotiate_url = Url::parse(hub_url)?;

    let mut path = negotiate_url.path().to_string();
    if path.ends_with('/') && path.len() > 1 {
        path.pop();
    }
    path.push_str("/negotiate");
    negotiate_url.set_path(&path);
    negotiate_url.set_fragment(None);

    let mut query_pairs: Vec<(String, String)> = negotiate_url
        .query_pairs()
        .filter(|(key, _)| key != "negotiateVersion" && key != "useStatefulReconnect")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    query_pairs.push((
        "negotiateVersion".into(),
        options.negotiate_version.to_string(),
    ));
    if options.use_stateful_reconnect {
        query_pairs.push(("useStatefulReconnect".into(), "true".into()));
    }

    negotiate_url.set_query(None);
    {
        let mut query = negotiate_url.query_pairs_mut();
        for (key, value) in &query_pairs {
            query.append_pair(key, value);
        }
    }

    Ok(negotiate_url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_negotiate_url_appends_negotiate_and_version() {
        let url = resolve_negotiate_url(
            "https://example.com/signalr?platform=pwa",
            &NegotiateOptions::new(),
        )
        .unwrap();

        assert!(url.starts_with("https://example.com/signalr/negotiate?"));
        assert!(url.contains("platform=pwa"));
        assert!(url.contains("negotiateVersion=1"));
    }

    #[test]
    fn build_websocket_url_sets_id_and_scheme() {
        let url = build_websocket_url(
            "https://example.com/signalr?platform=pwa",
            "connection-token",
            Some("jwt"),
        )
        .unwrap();

        assert!(url.starts_with("wss://example.com/signalr?"));
        assert!(url.contains("platform=pwa"));
        assert!(url.contains("id=connection-token"));
        assert!(url.contains("access_token=jwt"));
        assert!(!url.contains("negotiateVersion"));
    }

    #[test]
    fn websocket_token_prefers_connection_token() {
        let response = NegotiateResponse {
            url: None,
            access_token: None,
            error: None,
            connection_id: Some("id".into()),
            connection_token: Some("token".into()),
            negotiate_version: Some(1),
            use_stateful_reconnect: None,
        };

        assert_eq!(response.websocket_token().unwrap(), "token");
    }

    #[test]
    fn websocket_token_falls_back_to_connection_id() {
        let response = NegotiateResponse {
            url: None,
            access_token: None,
            error: None,
            connection_id: Some("id".into()),
            connection_token: None,
            negotiate_version: Some(0),
            use_stateful_reconnect: None,
        }
        .normalize();

        assert_eq!(response.websocket_token().unwrap(), "id");
    }
}
