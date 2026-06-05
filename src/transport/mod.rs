//! SignalR transport implementations.

mod send_queue;
mod websocket;

pub use send_queue::TransportSendQueue;
pub use websocket::{TransportMessage, WebSocketConnectOptions, WebSocketTransport};

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

pub(crate) fn format_data_detail(data: &TransportPayloadDetail, include_content: bool) -> String {
    match data {
        TransportPayloadDetail::Text(text) => {
            let mut detail = format!("String data of length {}", text.len());
            if include_content {
                detail.push_str(&format!(". Content: \"{text}\""));
            }
            detail
        }
        TransportPayloadDetail::Binary(bytes) => {
            let mut detail = format!("Binary data of length {}", bytes.len());
            if include_content {
                let hex = bytes
                    .iter()
                    .map(|value| format!("{value:02x}"))
                    .collect::<String>();
                detail.push_str(&format!(". Content: \"{hex}\""));
            }
            detail
        }
    }
}

pub(crate) enum TransportPayloadDetail<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

#[cfg(test)]
mod tests {
    use super::{TransportPayloadDetail, format_data_detail};

    #[test]
    fn format_data_detail_hides_content_by_default() {
        let detail = format_data_detail(&TransportPayloadDetail::Text("secret"), false);
        assert!(detail.contains("length 6"));
        assert!(!detail.contains("secret"));
    }

    #[test]
    fn format_data_detail_can_include_content() {
        let detail = format_data_detail(&TransportPayloadDetail::Text("secret"), true);
        assert!(detail.contains("secret"));
    }
}
