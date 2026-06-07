//! MessagePack+Brotli hub protocol (`messagepack+brotli`, version 2).

mod decoder;
mod parse;
mod write;

use std::sync::Arc;

use rmpv::Value;

pub use decoder::{BrotliDecoder, DefaultBrotliDecoder};

use crate::framing::BinaryMessageFormat;
use crate::hub::message::HubMessage;
use crate::hub::messagepack::brotli::parse::parse_message;
use crate::hub::messagepack::brotli::write::write_message as write_brotli_message;
use crate::hub::protocol::{HubProtocol, HubProtocolInput, ProtocolError};
use crate::transport::TransferFormat;

/// MessagePack hub protocol with Brotli-compressed field payloads.
#[derive(Clone)]
pub struct MessagePackBrotliProtocol {
    decoder: Arc<dyn BrotliDecoder>,
}

impl Default for MessagePackBrotliProtocol {
    fn default() -> Self {
        Self {
            decoder: Arc::new(DefaultBrotliDecoder),
        }
    }
}

impl MessagePackBrotliProtocol {
    pub fn new(decoder: Arc<dyn BrotliDecoder>) -> Self {
        Self { decoder }
    }
}

impl HubProtocol for MessagePackBrotliProtocol {
    fn name(&self) -> &str {
        "messagepack+brotli"
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
                "invalid input for MessagePack+Brotli hub protocol; expected binary".into(),
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

            let unpacked: Value =
                rmpv::decode::read_value(&mut frame.as_slice()).map_err(|error| {
                    ProtocolError::MessagePack(format!(
                        "failed to decode messagepack payload: {error}"
                    ))
                })?;

            let Value::Array(items) = unpacked else {
                return Err(ProtocolError::InvalidPayload("invalid payload".into()));
            };

            if items.is_empty() {
                return Err(ProtocolError::InvalidPayload(
                    "cannot encode message which is null".into(),
                ));
            }

            if let Some(message) = parse_message(&items, &self.decoder)? {
                messages.push(message);
            }
        }

        Ok(messages)
    }

    fn write_message(&self, message: &HubMessage) -> Result<Vec<u8>, ProtocolError> {
        write_brotli_message(message)
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::hub::message::InvocationMessage;

    #[test]
    fn parse_messages_decodes_real_update_state_payload() {
        let payload = STANDARD.decode(
            "lQKXAYDAq3VwZGF0ZVN0YXRl0ACRgqF0tG1haW4tcGFnZS1tYWluLXN0YWtloWSLoXLCoXOAonR1gKJ0b4CidHOAoW+AoW2BzgAYjGPeAB2hac4AGIxjoXPAonR1wKJ0b8CidHPAonNrwKJhc8ChdMCiaXTAomNswKJtdMCiaW/Ao3N0c8ChZMChbsCidG3Ao3N0YcChYcCic2PAoXDAoW/AonN0wKJyc8ChYoLOd0dzb4Khac53R3NvonAyzInOd0dzcIKhac53R3NwonAyzQEeonJiwKJiY26ibWPAoWfAom1uwKJybZCiaWXConN0vDIwMjUtMDYtMjBUMTI6NDY6MzkuNzAwNjk5Nlqjc3QyzgpJdy+Q",
        ).unwrap();

        let messages = MessagePackBrotliProtocol::default()
            .parse_messages(HubProtocolInput::Binary(&payload))
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], HubMessage::Invocation(_)));
        if let HubMessage::Invocation(InvocationMessage { target, .. }) = &messages[0] {
            assert_eq!(target, "updateState");
        }
    }

    #[test]
    fn round_trip_uncompressed_invocation() {
        let original = HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: Some("id".into()),
            target: "target".into(),
            arguments: vec![serde_json::json!("arg")],
            stream_ids: None,
        });

        let protocol = MessagePackBrotliProtocol::default();
        let encoded = protocol.write_message(&original).unwrap();
        let decoded = protocol
            .parse_messages(HubProtocolInput::Binary(&encoded))
            .unwrap();

        assert_eq!(decoded.len(), 1);
        if let HubMessage::Invocation(message) = &decoded[0] {
            assert_eq!(message.target, "target");
            assert_eq!(message.arguments, vec![serde_json::json!("arg")]);
        } else {
            panic!("expected invocation message");
        }
    }

    #[test]
    fn parse_messages_rejects_text_input() {
        let error = MessagePackBrotliProtocol::default()
            .parse_messages(HubProtocolInput::Text("{}"))
            .expect_err("text input should fail");
        assert!(matches!(error, ProtocolError::InvalidInput(_)));
    }
}
