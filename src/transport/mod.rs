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
    parsed.query_pairs_mut().append_pair("access_token", token);
    Ok(parsed.to_string())
}

/// `url` for logs: the value of `access_token` is replaced with `***`.
pub(crate) fn redact_access_token(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return "<unparsable url>".into();
    };
    if !parsed.query_pairs().any(|(key, _)| key == "access_token") {
        return url.to_string();
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "access_token" {
                "***".to_string()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    parsed.query_pairs_mut().clear().extend_pairs(pairs);
    parsed.to_string()
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
    use super::{TransportPayloadDetail, format_data_detail, redact_access_token};

    #[test]
    fn redact_access_token_hides_only_the_token() {
        let url = "wss://host/hub?room=1&id=abc&access_token=secret.jwt";
        let redacted = redact_access_token(url);
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(
            redacted.contains("access_token=***") || redacted.contains("access_token=%2A%2A%2A")
        );
        assert!(redacted.contains("id=abc") && redacted.contains("room=1"));
        assert_eq!(
            redact_access_token("wss://host/hub?id=abc"),
            "wss://host/hub?id=abc"
        );
    }

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
