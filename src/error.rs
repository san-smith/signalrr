use futures_channel::oneshot::Canceled;
use thiserror::Error;

/// Errors that can occur in the SignalR client.
#[derive(Error, Debug)]
pub enum Error {
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),

    #[error("negotiate failed: {0}")]
    Negotiate(String),

    #[error("handshake failed: {0}")]
    Handshake(String),

    #[error("framing error: {0}")]
    Framing(#[from] FramingError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("not connected")]
    NotConnected,

    #[error("invocation canceled")]
    Canceled,

    #[error("invocation error: {0}")]
    Invocation(String),

    #[error("invalid connection state: {0}")]
    InvalidState(String),
}

/// Errors during text/binary message framing.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum FramingError {
    #[error("message is incomplete")]
    IncompleteMessage,

    #[error("cannot read message size")]
    CannotReadSize,

    #[error("messages bigger than 2GB are not supported")]
    MessageTooLarge,
}

impl From<Canceled> for Error {
    fn from(_: Canceled) -> Self {
        Error::Canceled
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error::Invocation(message)
    }
}
