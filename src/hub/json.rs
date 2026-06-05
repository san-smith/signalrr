//! JSON hub protocol implementation.

use crate::framing::TextMessageFormat;
use crate::hub::message::HubMessage;
use crate::hub::protocol::{HubProtocol, HubProtocolInput, ProtocolError};
use crate::transport::TransferFormat;

/// JSON hub protocol (`json`, version 2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JsonProtocol;

impl HubProtocol for JsonProtocol {
    fn name(&self) -> &str {
        "json"
    }

    fn version(&self) -> i32 {
        2
    }

    fn transfer_format(&self) -> TransferFormat {
        TransferFormat::Text
    }

    fn parse_messages(&self, input: HubProtocolInput<'_>) -> Result<Vec<HubMessage>, ProtocolError> {
        let HubProtocolInput::Text(text) = input else {
            return Err(ProtocolError::InvalidInput(
                "invalid input for JSON hub protocol; expected text".into(),
            ));
        };

        let mut messages = Vec::new();
        for message in TextMessageFormat::parse(text)? {
            let json_value: serde_json::Value = serde_json::from_str(&message)?;
            if let Some(parsed) = HubMessage::from_json_value(&json_value) {
                messages.push(parsed);
            }
        }

        Ok(messages)
    }

    fn write_message(&self, message: &HubMessage) -> Result<Vec<u8>, ProtocolError> {
        let json = serde_json::to_string(&message.to_json_value())?;
        Ok(TextMessageFormat::write(&json).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::RECORD_SEPARATOR;
    use crate::hub::message::{CompletionMessage, PingMessage};

    #[test]
    fn parse_messages_returns_completion_message() {
        let input = format!(
            r#"{{"type":3,"invocationId":"abc","result":1514805840000,"headers":{{}}}}{RECORD_SEPARATOR}"#
        );
        let messages = JsonProtocol
            .parse_messages(HubProtocolInput::Text(&input))
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], HubMessage::Completion(_)));
    }

    #[test]
    fn parse_messages_ignores_unknown_type() {
        let input = format!(r#"{{"type":228}}{RECORD_SEPARATOR}"#);
        let messages = JsonProtocol
            .parse_messages(HubProtocolInput::Text(&input))
            .unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn parse_messages_rejects_binary_input() {
        let error = JsonProtocol
            .parse_messages(HubProtocolInput::Binary(&[1, 2, 3]))
            .expect_err("binary input should fail");
        assert!(matches!(error, ProtocolError::InvalidInput(_)));
    }

    #[test]
    fn write_message_serializes_ping() {
        let bytes = JsonProtocol
            .write_message(&HubMessage::Ping(PingMessage))
            .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            format!(r#"{{"type":6}}{RECORD_SEPARATOR}"#)
        );
    }

    #[test]
    fn round_trip_completion_message() {
        let original = HubMessage::Completion(CompletionMessage {
            headers: Default::default(),
            invocation_id: "abc".into(),
            error: None,
            result: Some(serde_json::json!(1514805840000_i64)),
        });

        let encoded = JsonProtocol.write_message(&original).unwrap();
        let decoded = JsonProtocol
            .parse_messages(HubProtocolInput::Text(std::str::from_utf8(&encoded).unwrap()))
            .unwrap();

        assert_eq!(decoded, vec![original]);
    }
}
