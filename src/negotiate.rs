//! HTTP negotiate step before opening the WebSocket transport.

use crate::error::Error;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

const DEFAULT_NEGOTIATE_VERSION: u32 = 1;

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
}

/// Options for the negotiate request.
#[derive(Debug, Clone, Default)]
pub struct NegotiateOptions {
    pub negotiate_version: u32,
    pub use_stateful_reconnect: bool,
    pub headers: Vec<(String, String)>,
}

impl NegotiateOptions {
    pub fn new() -> Self {
        Self {
            negotiate_version: DEFAULT_NEGOTIATE_VERSION,
            ..Self::default()
        }
    }
}

/// Performs the negotiate request and returns the parsed response.
pub async fn negotiate(
    hub_url: &str,
    options: &NegotiateOptions,
) -> Result<NegotiateResponse, Error> {
    let negotiate_url = resolve_negotiate_url(hub_url, options)?;
    let client = Client::new();
    let mut request = client
        .post(negotiate_url)
        .header("Accept", "application/json");

    for (name, value) in &options.headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(Error::Negotiate(format!(
            "unexpected status code: {}",
            response.status()
        )));
    }

    let mut negotiate_response: NegotiateResponse = response.json().await?;
    if negotiate_response.negotiate_version.unwrap_or(0) < 1 {
        negotiate_response.connection_token = negotiate_response
            .connection_token
            .or(negotiate_response.connection_id.clone());
    }

    if negotiate_response.error.is_some() {
        return Err(Error::Negotiate(
            negotiate_response.error.clone().unwrap_or_default(),
        ));
    }

    Ok(negotiate_response)
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
        };

        assert_eq!(response.websocket_token().unwrap(), "id");
    }
}
