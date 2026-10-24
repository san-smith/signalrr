//! MessagePack+Brotli hub protocol parse helpers.

use std::sync::Arc;

use rmpv::Value;

use crate::hub::arguments::Arguments;
use crate::hub::message::{
    CompletionMessage, HubMessage, InvocationMessage, MessageType, StreamInvocationMessage,
    StreamItemMessage,
};
use crate::hub::messagepack::brotli::decoder::BrotliDecoder;
use crate::hub::messagepack::parse::{
    create_ack_message, create_close_message, create_message_headers, create_ping_message,
    create_sequence_message, optional_string, value_to_i64, value_to_json, value_to_json_array,
    value_to_string,
};
use crate::hub::protocol::ProtocolError;

const ERROR_RESULT: i64 = 1;
const VOID_RESULT: i64 = 2;
const NON_VOID_RESULT: i64 = 3;

/// Parses a decoded message; `raw_arguments` are the encoded invocation arguments from
/// [`decode_message`](crate::hub::messagepack::parse::decode_message).
pub fn parse_message(
    data: &[Value],
    raw_arguments: Option<&[u8]>,
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<Option<HubMessage>, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::InvalidPayload("invalid payload".into()));
    }

    let message_type = value_to_i64(&data[0])?;
    let Some(message_type) = MessageType::from_i32(message_type as i32) else {
        return Ok(None);
    };

    let message = match message_type {
        MessageType::Invocation => {
            HubMessage::Invocation(create_invocation_message(data, raw_arguments, decoder)?)
        }
        MessageType::StreamInvocation => {
            HubMessage::StreamInvocation(create_stream_invocation_message(data, decoder)?)
        }
        MessageType::StreamItem => {
            HubMessage::StreamItem(create_stream_item_message(data, decoder)?)
        }
        MessageType::Completion => {
            HubMessage::Completion(create_completion_message(data, decoder)?)
        }
        MessageType::Ping => HubMessage::Ping(create_ping_message(data)?),
        MessageType::Close => HubMessage::Close(create_close_message(data)?),
        MessageType::Ack => HubMessage::Ack(create_ack_message(data)?),
        MessageType::Sequence => HubMessage::Sequence(create_sequence_message(data)?),
        MessageType::CancelInvocation => return Ok(None),
    };

    Ok(Some(message))
}

fn create_completion_message(
    data: &[Value],
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<CompletionMessage, ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Completion message".into(),
        ));
    }

    let headers = create_message_headers(data)?;
    let result_kind = value_to_i64(&data[3])?;
    if result_kind != VOID_RESULT && data.len() < 5 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Completion message".into(),
        ));
    }

    let invocation_id = value_to_string(&data[2])?;
    if result_kind == ERROR_RESULT || result_kind == NON_VOID_RESULT {
        let is_compressed = data.get(4).and_then(|value| value.as_i64()) == Some(1);
        let unpacked = unpack_data(data.get(5), is_compressed, decoder)?;
        if result_kind == ERROR_RESULT {
            return Ok(CompletionMessage {
                headers,
                invocation_id,
                error: unpacked
                    .as_ref()
                    .and_then(|value| value.as_str().map(str::to_string)),
                result: None,
            });
        }

        return Ok(CompletionMessage {
            headers,
            invocation_id,
            error: None,
            result: unpacked.map(|value| value_to_json(&value)),
        });
    }

    Ok(CompletionMessage {
        headers,
        invocation_id,
        error: None,
        result: None,
    })
}

fn create_stream_item_message(
    data: &[Value],
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<StreamItemMessage, ProtocolError> {
    if data.len() < 5 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for StreamItem message".into(),
        ));
    }

    let is_compressed = data.get(3).and_then(|value| value.as_i64()) == Some(1);
    let item = unpack_data(data.get(4), is_compressed, decoder)?
        .map(|value| value_to_json(&value))
        .unwrap_or(serde_json::Value::Null);

    Ok(StreamItemMessage {
        headers: create_message_headers(data)?,
        invocation_id: optional_string(&data[2])?,
        item,
    })
}

fn create_invocation_message(
    data: &[Value],
    raw_arguments: Option<&[u8]>,
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<InvocationMessage, ProtocolError> {
    if data.len() < 5 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Invocation message".into(),
        ));
    }

    let is_compressed = data.get(4).and_then(|value| value.as_i64()) == Some(1);
    let arguments = match raw_arguments {
        None => Arguments::default(),
        Some(raw) if is_compressed => {
            let packed = rmpv::decode::read_value(&mut &raw[..]).map_err(|error| {
                ProtocolError::MessagePack(format!("failed to decode arguments: {error}"))
            })?;
            let bytes = value_to_bytes(&packed).ok_or_else(|| {
                ProtocolError::InvalidPayload("compressed arguments are not binary".into())
            })?;
            Arguments::from_messagepack(decoder.decode(&bytes)?)?
        }
        Some(raw) => Arguments::from_messagepack(raw.to_vec())?,
    };

    Ok(InvocationMessage {
        headers: create_message_headers(data)?,
        invocation_id: optional_string(&data[2])?,
        target: value_to_string(&data[3])?,
        arguments,
        stream_ids: Some(Vec::new()),
    })
}

fn create_stream_invocation_message(
    data: &[Value],
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<StreamInvocationMessage, ProtocolError> {
    if data.len() < 6 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for StreamInvocation message".into(),
        ));
    }

    let is_compressed = data.get(4).and_then(|value| value.as_i64()) == Some(1);
    let arguments = match unpack_data(data.get(5), is_compressed, decoder)? {
        Some(value) => value_to_json_array(&value)?,
        None => Vec::new(),
    };

    Ok(StreamInvocationMessage {
        headers: create_message_headers(data)?,
        invocation_id: value_to_string(&data[2])?,
        target: value_to_string(&data[3])?,
        arguments,
        stream_ids: Some(Vec::new()),
    })
}

pub fn unpack_data(
    data: Option<&Value>,
    is_compressed: bool,
    decoder: &Arc<dyn BrotliDecoder>,
) -> Result<Option<Value>, ProtocolError> {
    let Some(data) = data else {
        return Ok(None);
    };

    if !is_compressed {
        return Ok(Some(data.clone()));
    }

    let Some(bytes) = value_to_bytes(data) else {
        return Ok(None);
    };

    let decoded = decoder.decode(&bytes)?;
    let value = rmpv::decode::read_value(&mut decoded.as_slice()).map_err(|error| {
        ProtocolError::MessagePack(format!("failed to decode brotli payload: {error}"))
    })?;

    Ok(Some(value))
}

fn value_to_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Binary(bytes) => Some(bytes.clone()),
        Value::Array(items) => {
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                let byte = item.as_u64()? as u8;
                bytes.push(byte);
            }
            Some(bytes)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::message::{AckMessage, PingMessage};
    use crate::hub::messagepack::brotli::decoder::DefaultBrotliDecoder;

    fn encode_value(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value).unwrap();
        bytes
    }

    fn decoder() -> Arc<dyn BrotliDecoder> {
        Arc::new(DefaultBrotliDecoder)
    }

    fn identity_decoder() -> Arc<dyn BrotliDecoder> {
        Arc::new(IdentityDecoder)
    }

    struct IdentityDecoder;

    impl BrotliDecoder for IdentityDecoder {
        fn decode(&self, data: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            Ok(data.to_vec())
        }
    }

    #[test]
    fn create_completion_message_handles_error_non_void_and_void() {
        let error = create_completion_message(
            &[
                Value::from(MessageType::Completion as i32),
                Value::Map(vec![]),
                Value::String("invocationId".into()),
                Value::from(ERROR_RESULT),
                Value::from(0),
                Value::String("error".into()),
            ],
            &identity_decoder(),
        )
        .unwrap();
        assert_eq!(error.invocation_id, "invocationId");
        assert_eq!(error.error.as_deref(), Some("error"));

        let result = create_completion_message(
            &[
                Value::from(MessageType::Completion as i32),
                Value::Map(vec![]),
                Value::String("invocationId".into()),
                Value::from(NON_VOID_RESULT),
                Value::from(0),
                Value::String("result".into()),
            ],
            &identity_decoder(),
        )
        .unwrap();
        assert_eq!(
            result.result,
            Some(serde_json::Value::String("result".into()))
        );

        let void = create_completion_message(
            &[
                Value::from(MessageType::Completion as i32),
                Value::Map(vec![]),
                Value::String("invocationId".into()),
                Value::from(VOID_RESULT),
            ],
            &identity_decoder(),
        )
        .unwrap();
        assert!(void.error.is_none());
        assert!(void.result.is_none());
    }

    #[test]
    fn create_stream_item_and_invocation_messages() {
        let item = create_stream_item_message(
            &[
                Value::from(MessageType::StreamItem as i32),
                Value::Map(vec![]),
                Value::String("invocationId".into()),
                Value::from(0),
                Value::String("item".into()),
            ],
            &identity_decoder(),
        )
        .unwrap();
        assert_eq!(item.item, serde_json::Value::String("item".into()));

        let invocation = create_invocation_message(
            &[
                Value::from(MessageType::Invocation as i32),
                Value::Map(vec![]),
                Value::String("invocationId".into()),
                Value::String("target".into()),
                Value::from(0),
                Value::Nil,
            ],
            Some(&encode_value(&Value::Array(vec![
                Value::String("arg1".into()),
                Value::String("arg2".into()),
            ]))),
            &identity_decoder(),
        )
        .unwrap();
        assert_eq!(invocation.target, "target");
        assert_eq!(invocation.arguments.to_json().unwrap().len(), 2);
    }

    #[test]
    fn unpack_data_decompresses_msgpack_payload() {
        let payload = {
            let mut buffer = Vec::new();
            rmpv::encode::write_value(&mut buffer, &Value::String("hello world".into())).unwrap();
            buffer
        };
        let compressed = {
            let mut output = Vec::new();
            {
                let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 11, 22);
                std::io::Write::write_all(&mut writer, &payload).unwrap();
            }
            output
        };

        let result = unpack_data(Some(&Value::Binary(compressed)), true, &decoder()).unwrap();
        assert_eq!(result, Some(Value::String("hello world".into())));
    }

    #[test]
    fn parse_message_handles_common_message_types() {
        let decoder = identity_decoder();
        assert!(matches!(
            parse_message(
                &[
                    Value::from(MessageType::Invocation as i32),
                    Value::Map(vec![]),
                    Value::String("id".into()),
                    Value::String("target".into()),
                    Value::from(0),
                    Value::Nil,
                ],
                Some(&encode_value(&Value::Array(vec![]))),
                &decoder,
            )
            .unwrap(),
            Some(HubMessage::Invocation(_))
        ));
        assert!(matches!(
            parse_message(&[Value::from(MessageType::Ping as i32)], None, &decoder).unwrap(),
            Some(HubMessage::Ping(PingMessage))
        ));
        assert!(matches!(
            parse_message(
                &[Value::from(MessageType::Close as i32), Value::Nil],
                None,
                &decoder,
            )
            .unwrap(),
            Some(HubMessage::Close(_))
        ));
        assert!(matches!(
            parse_message(
                &[Value::from(MessageType::Ack as i32), Value::from(1)],
                None,
                &decoder,
            )
            .unwrap(),
            Some(HubMessage::Ack(AckMessage { sequence_id: 1 }))
        ));
        assert!(
            parse_message(&[Value::from(99)], None, &decoder)
                .unwrap()
                .is_none()
        );
    }
}
