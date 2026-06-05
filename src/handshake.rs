//! SignalR connection handshake (always JSON, even for binary hub protocols).

use crate::error::{Error, FramingError};
use crate::framing::{RECORD_SEPARATOR, TextMessageFormat};
use serde::{Deserialize, Serialize};

/// Client handshake request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandshakeRequest {
    pub protocol: String,
    pub version: i32,
}

/// Server handshake response payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HandshakeResponse {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, rename = "minorVersion")]
    pub minor_version: Option<i32>,
}

/// Remaining data after the handshake message in the transport buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemainingData {
    Text(String),
    Bytes(Vec<u8>),
}

/// Parsed handshake response and any trailing transport data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeResult {
    pub response: HandshakeResponse,
    pub remaining: Option<RemainingData>,
}

/// SignalR handshake encoder/decoder.
#[derive(Debug, Default, Clone, Copy)]
pub struct HandshakeProtocol;

impl HandshakeProtocol {
    /// Serializes a handshake request as JSON with a record separator suffix.
    pub fn write_request(&self, request: &HandshakeRequest) -> Result<String, Error> {
        let json = serde_json::to_string(request)?;
        Ok(TextMessageFormat::write(&json))
    }

    /// Parses a handshake response from text or binary transport data.
    pub fn parse_response(&self, data: HandshakeInput<'_>) -> Result<HandshakeResult, Error> {
        let (message_data, remaining) = match data {
            HandshakeInput::Text(text) => split_text_handshake(text)?,
            HandshakeInput::Binary(bytes) => split_binary_handshake(bytes)?,
        };

        let messages = TextMessageFormat::parse(&message_data).map_err(Error::Framing)?;
        let response_json = messages
            .first()
            .ok_or_else(|| Error::Handshake("empty handshake response".into()))?;
        let response: HandshakeResponse = serde_json::from_str(response_json)?;

        Ok(HandshakeResult {
            response,
            remaining,
        })
    }
}

/// Handshake input from the transport layer.
pub enum HandshakeInput<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

fn split_text_handshake(text: &str) -> Result<(String, Option<RemainingData>), Error> {
    let separator_index = text
        .find(RECORD_SEPARATOR)
        .ok_or(Error::Framing(FramingError::IncompleteMessage))?;

    let response_length = separator_index + RECORD_SEPARATOR.len_utf8();
    let message_data = text[..response_length].to_string();
    let remaining = if text.len() > response_length {
        Some(RemainingData::Text(text[response_length..].to_string()))
    } else {
        None
    };

    Ok((message_data, remaining))
}

fn split_binary_handshake(bytes: &[u8]) -> Result<(String, Option<RemainingData>), Error> {
    let separator_index = bytes
        .iter()
        .position(|&byte| byte == 0x1e)
        .ok_or(Error::Framing(FramingError::IncompleteMessage))?;

    let response_length = separator_index + 1;
    let message_data = String::from_utf8(bytes[..response_length].to_vec())
        .map_err(|error| Error::Handshake(error.to_string()))?;
    let remaining = if bytes.len() > response_length {
        Some(RemainingData::Bytes(bytes[response_length..].to_vec()))
    } else {
        None
    };

    Ok((message_data, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::RECORD_SEPARATOR;

    #[test]
    fn write_request_serializes_json_with_separator() {
        let protocol = HandshakeProtocol;
        let request = HandshakeRequest {
            protocol: "protocol".into(),
            version: 1,
        };

        assert_eq!(
            protocol.write_request(&request).unwrap(),
            format!(r#"{{"protocol":"protocol","version":1}}{RECORD_SEPARATOR}"#)
        );
    }

    #[test]
    fn parse_empty_json_response() {
        let input = format!("{{}}{RECORD_SEPARATOR}");
        let result = HandshakeProtocol
            .parse_response(HandshakeInput::Text(&input))
            .unwrap();

        assert!(result.response.error.is_none());
        assert!(result.response.minor_version.is_none());
        assert!(result.remaining.is_none());
    }

    #[test]
    fn parse_empty_binary_response() {
        let input = vec![b'{', b'}', 0x1e];
        let result = HandshakeProtocol
            .parse_response(HandshakeInput::Binary(&input))
            .unwrap();

        assert!(result.response.error.is_none());
        assert!(result.response.minor_version.is_none());
        assert!(result.remaining.is_none());
    }

    #[test]
    fn parse_text_response_with_remaining_data() {
        let input = format!(r#"{{"error":"error","minorVersion":1}}{RECORD_SEPARATOR}lol"#);
        let result = HandshakeProtocol
            .parse_response(HandshakeInput::Text(&input))
            .unwrap();

        assert_eq!(result.response.error.as_deref(), Some("error"));
        assert_eq!(result.response.minor_version, Some(1));
        assert_eq!(
            result.remaining,
            Some(RemainingData::Text("lol".into()))
        );
    }

    #[test]
    fn parse_binary_response_with_remaining_data() {
        let input = vec![
            b'{', b'"', b'e', b'r', b'r', b'o', b'r', b'"', b':', b'"', b'e', b'r', b'r',
            b'o', b'r', b'"', b',', b'"', b'm', b'i', b'n', b'o', b'r', b'V', b'e', b'r',
            b's', b'i', b'o', b'n', b'"', b':', b'1', b'}', 0x1e, b'l', b'o', b'l',
        ];
        let result = HandshakeProtocol
            .parse_response(HandshakeInput::Binary(&input))
            .unwrap();

        assert_eq!(result.response.error.as_deref(), Some("error"));
        assert_eq!(result.response.minor_version, Some(1));
        assert_eq!(
            result.remaining,
            Some(RemainingData::Bytes(vec![b'l', b'o', b'l']))
        );
    }
}
