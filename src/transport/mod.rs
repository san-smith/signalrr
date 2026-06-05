//! SignalR transport implementations.

mod websocket;

pub use websocket::{TransportMessage, WebSocketTransport};

use crate::error::Error;

/// Wire format selected during negotiate/handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferFormat {
    #[default]
    Text,
    Binary,
}

/// Appends `access_token` as a query parameter when present.
pub(crate) fn append_access_token(url: &str, access_token: Option<&str>) -> Result<String, Error> {
    let Some(token) = access_token.filter(|token| !token.is_empty()) else {
        return Ok(url.to_string());
    };

    let mut parsed = url::Url::parse(url)?;
    parsed
        .query_pairs_mut()
        .append_pair("access_token", token);
    Ok(parsed.to_string())
}
