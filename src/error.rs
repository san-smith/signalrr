use futures_channel::oneshot::Canceled;
use thiserror::Error;

/// Errors that can occur in the SignalR client.
#[derive(Error, Debug)]
pub enum SignalRError {
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("Protocol error: {0}")]
    Protocol(#[from] crate::protocol::SignalRProtocolError),

    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Invocation canceled")]
    Canceled,

    #[error("Invocation error: {0}")]
    InvocationError(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

impl From<Canceled> for SignalRError {
    fn from(_: Canceled) -> Self {
        SignalRError::Canceled
    }
}

impl From<String> for SignalRError {
    fn from(s: String) -> Self {
        SignalRError::InvocationError(s)
    }
}
