//! HTTP negotiate step before opening the WebSocket transport.

use std::time::Duration;

use crate::access_token::{self, AccessTokenFactory};
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
///
/// The token is fixed: a `401 Unauthorized` response fails with [`Error::Unauthorized`].
/// Use [`negotiate_with_token_factory`] to fetch a fresh token and retry instead.
pub async fn negotiate_with_client(
    hub_url: &str,
    options: &NegotiateOptions,
    client: &Client,
    access_token: Option<&str>,
) -> Result<NegotiateResult, Error> {
    let token = TokenSource::Fixed(access_token.map(str::to_owned));
    negotiate_with_source(hub_url, options, client, token).await
}

/// Performs negotiate with tokens from `factory`, like the official SignalR clients.
///
/// The token is requested before the first request. When a request gets
/// `401 Unauthorized`, a fresh token is requested and that request is sent once more; a
/// second `401` fails with [`Error::Unauthorized`]. Once the server issues its own
/// `accessToken` during a redirect, that token is used as is and `factory` is not called
/// again.
pub async fn negotiate_with_token_factory(
    hub_url: &str,
    options: &NegotiateOptions,
    client: &Client,
    factory: Option<&AccessTokenFactory>,
) -> Result<NegotiateResult, Error> {
    let token = match factory {
        Some(factory) => TokenSource::Factory {
            factory,
            token: access_token::resolve(Some(factory)).await?,
        },
        None => TokenSource::Fixed(None),
    };
    negotiate_with_source(hub_url, options, client, token).await
}

enum TokenSource<'a> {
    Fixed(Option<String>),
    Factory {
        factory: &'a AccessTokenFactory,
        token: Option<String>,
    },
}

impl TokenSource<'_> {
    fn token(&self) -> Option<&str> {
        match self {
            Self::Fixed(token) | Self::Factory { token, .. } => token.as_deref(),
        }
    }

    fn into_token(self) -> Option<String> {
        match self {
            Self::Fixed(token) | Self::Factory { token, .. } => token,
        }
    }

    /// Fetches a fresh token; returns `false` when the token cannot be refreshed.
    async fn refresh(&mut self) -> Result<bool, Error> {
        match self {
            Self::Fixed(_) => Ok(false),
            Self::Factory { factory, token } => {
                *token = access_token::resolve(Some(*factory)).await?;
                Ok(true)
            }
        }
    }
}

async fn negotiate_with_source(
    hub_url: &str,
    options: &NegotiateOptions,
    client: &Client,
    mut token: TokenSource<'_>,
) -> Result<NegotiateResult, Error> {
    let mut current_url = hub_url.to_string();
    let mut redirects = 0;

    loop {
        let response = match negotiate_once(&current_url, options, client, token.token()).await {
            Err(Error::Unauthorized(_)) if token.refresh().await? => {
                negotiate_once(&current_url, options, client, token.token()).await?
            }
            result => result?,
        };

        if let Some(error) = &response.error {
            return Err(Error::Negotiate(error.clone()));
        }

        if (response.use_stateful_reconnect.unwrap_or(false)) && !options.use_stateful_reconnect {
            return Err(Error::Negotiate(
                "client did not negotiate stateful reconnect but the server did".into(),
            ));
        }

        if let Some(server_token) = &response.access_token {
            token = TokenSource::Fixed(Some(server_token.clone()));
        }

        let Some(redirect_url) = response.url.clone() else {
            return Ok(NegotiateResult {
                response,
                access_token: token.into_token(),
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
    match response.status() {
        reqwest::StatusCode::OK => {}
        reqwest::StatusCode::UNAUTHORIZED => {
            return Err(Error::Unauthorized(
                "negotiate returned 401 Unauthorized".into(),
            ));
        }
        status => {
            return Err(Error::Negotiate(format!(
                "unexpected status code returned from negotiate: {status}"
            )));
        }
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn negotiate_ok() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "connectionId": "id",
            "connectionToken": "connection-token",
            "negotiateVersion": 1
        }))
    }

    /// Factory returning `token-1`, `token-2`, ... and counting its calls.
    fn counting_factory() -> (AccessTokenFactory, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let factory = crate::access_token_factory(move || {
            let call = counter.fetch_add(1, Ordering::SeqCst) + 1;
            async move { Ok(format!("token-{call}")) }
        });
        (factory, calls)
    }

    async fn requests_to(server: &MockServer) -> usize {
        server.received_requests().await.unwrap_or_default().len()
    }

    #[tokio::test]
    async fn token_factory_retries_once_after_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .and(header("Authorization", "Bearer token-1"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .and(header("Authorization", "Bearer token-2"))
            .respond_with(negotiate_ok())
            .mount(&server)
            .await;
        let (factory, calls) = counting_factory();

        let result = negotiate_with_token_factory(
            &format!("{}/hub", server.uri()),
            &NegotiateOptions::new(),
            &Client::new(),
            Some(&factory),
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(requests_to(&server).await, 2);
        assert_eq!(result.access_token.as_deref(), Some("token-2"));
    }

    #[tokio::test]
    async fn token_factory_fails_after_second_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let (factory, calls) = counting_factory();

        let error = negotiate_with_token_factory(
            &format!("{}/hub", server.uri()),
            &NegotiateOptions::new(),
            &Client::new(),
            Some(&factory),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Unauthorized(_)), "{error:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(requests_to(&server).await, 2);
    }

    #[tokio::test]
    async fn token_factory_is_called_once_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .and(header("Authorization", "Bearer token-1"))
            .respond_with(negotiate_ok())
            .mount(&server)
            .await;
        let (factory, calls) = counting_factory();

        let result = negotiate_with_token_factory(
            &format!("{}/hub", server.uri()),
            &NegotiateOptions::new(),
            &Client::new(),
            Some(&factory),
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(requests_to(&server).await, 1);
        assert_eq!(result.access_token.as_deref(), Some("token-1"));
    }

    #[tokio::test]
    async fn fixed_token_is_not_retried_after_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let error = negotiate_with_client(
            &format!("{}/hub", server.uri()),
            &NegotiateOptions::new(),
            &Client::new(),
            Some("fixed"),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Unauthorized(_)), "{error:?}");
        assert_eq!(requests_to(&server).await, 1);
    }

    #[tokio::test]
    async fn server_issued_token_is_not_refreshed_after_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hub/negotiate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "url": format!("{}/redirected", server.uri()),
                "accessToken": "server-token"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/redirected/negotiate"))
            .and(header("Authorization", "Bearer server-token"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let (factory, calls) = counting_factory();

        let error = negotiate_with_token_factory(
            &format!("{}/hub", server.uri()),
            &NegotiateOptions::new(),
            &Client::new(),
            Some(&factory),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Unauthorized(_)), "{error:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(requests_to(&server).await, 2);
    }

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
