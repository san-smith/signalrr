//! MessagePack hub protocol implementation.

mod parse;
mod write;

pub(crate) use parse::value_to_json_array;
pub(crate) use write::json_array_value;

#[cfg(feature = "messagepack-brotli")]
pub mod brotli;

use crate::framing::BinaryMessageFormat;
use crate::hub::message::HubMessage;
use crate::hub::messagepack::parse::{decode_message, parse_message};
use crate::hub::messagepack::write::write_message as write_hub_message;
use crate::hub::protocol::{HubProtocol, HubProtocolInput, ProtocolError};
use crate::transport::TransferFormat;

/// MessagePack hub protocol (`messagepack`, version 2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MessagePackProtocol;

impl HubProtocol for MessagePackProtocol {
    fn name(&self) -> &str {
        "messagepack"
    }

    fn version(&self) -> i32 {
        2
    }

    fn transfer_format(&self) -> TransferFormat {
        TransferFormat::Binary
    }

    fn parse_messages(
        &self,
        input: HubProtocolInput<'_>,
    ) -> Result<Vec<HubMessage>, ProtocolError> {
        let HubProtocolInput::Binary(bytes) = input else {
            return Err(ProtocolError::InvalidInput(
                "invalid input for MessagePack hub protocol; expected binary".into(),
            ));
        };

        let frames = BinaryMessageFormat::parse(bytes)?;
        if frames.is_empty() {
            return Err(ProtocolError::InvalidPayload(
                "cannot encode message which is null".into(),
            ));
        }

        let mut messages = Vec::new();
        for frame in frames {
            if frame.is_empty() {
                return Err(ProtocolError::InvalidPayload(
                    "cannot encode message which is null".into(),
                ));
            }

            let (items, raw_arguments) = decode_message(&frame, 4)?;
            if let Some(message) = parse_message(&items, raw_arguments)? {
                messages.push(message);
            }
        }

        Ok(messages)
    }

    fn write_message(&self, message: &HubMessage) -> Result<Vec<u8>, ProtocolError> {
        write_hub_message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::message::{CompletionMessage, PingMessage};

    #[test]
    fn round_trip_ping_message() {
        let original = HubMessage::Ping(PingMessage);
        let encoded = MessagePackProtocol.write_message(&original).unwrap();
        let decoded = MessagePackProtocol
            .parse_messages(HubProtocolInput::Binary(&encoded))
            .unwrap();
        assert_eq!(decoded, vec![original]);
    }

    #[test]
    fn round_trip_completion_message() {
        let original = HubMessage::Completion(CompletionMessage {
            headers: Default::default(),
            invocation_id: "1".into(),
            error: None,
            result: None,
        });
        let encoded = MessagePackProtocol.write_message(&original).unwrap();
        let decoded = MessagePackProtocol
            .parse_messages(HubProtocolInput::Binary(&encoded))
            .unwrap();
        assert_eq!(decoded, vec![original]);
    }

    #[test]
    fn parse_messages_rejects_text_input() {
        let error = MessagePackProtocol
            .parse_messages(HubProtocolInput::Text("{}"))
            .expect_err("text input should fail");
        assert!(matches!(error, ProtocolError::InvalidInput(_)));
    }
}
