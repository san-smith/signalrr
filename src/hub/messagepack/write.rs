//! MessagePack hub protocol write helpers.

use std::collections::HashMap;

use rmpv::Value;

use crate::framing::BinaryMessageFormat;
use crate::hub::message::{
    AckMessage, CancelInvocationMessage, CompletionMessage, HubMessage, InvocationMessage,
    MessageType, SequenceMessage, StreamInvocationMessage, StreamItemMessage,
};
use crate::hub::protocol::ProtocolError;

const ERROR_RESULT: i64 = 1;
const VOID_RESULT: i64 = 2;
const NON_VOID_RESULT: i64 = 3;

pub fn write_message(message: &HubMessage) -> Result<Vec<u8>, ProtocolError> {
    match message {
        HubMessage::Invocation(message) => write_invocation(message),
        HubMessage::StreamInvocation(message) => write_stream_invocation(message),
        HubMessage::StreamItem(message) => write_stream_item(message),
        HubMessage::Completion(message) => write_completion(message),
        HubMessage::Ping(_) => write_ping(),
        HubMessage::CancelInvocation(message) => write_cancel_invocation(message),
        HubMessage::Close(_) => write_close(),
        HubMessage::Ack(message) => write_ack(*message),
        HubMessage::Sequence(message) => write_sequence(*message),
    }
}

pub fn write_ping() -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[Value::from(MessageType::Ping as i32)])
}

pub fn write_cancel_invocation(message: &CancelInvocationMessage) -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[
        Value::from(MessageType::CancelInvocation as i32),
        headers_value(&message.headers),
        Value::String(message.invocation_id.clone().into()),
    ])
}

pub fn write_completion(message: &CompletionMessage) -> Result<Vec<u8>, ProtocolError> {
    let result_kind = if message.error.is_some() {
        ERROR_RESULT
    } else if message.result.is_some() {
        NON_VOID_RESULT
    } else {
        VOID_RESULT
    };

    let mut payload = vec![
        Value::from(MessageType::Completion as i32),
        headers_value(&message.headers),
        Value::String(message.invocation_id.clone().into()),
        Value::from(result_kind),
    ];

    match result_kind {
        ERROR_RESULT => payload.push(json_to_value(message.error.as_ref().map(|value| {
            serde_json::Value::String(value.clone())
        }))),
        NON_VOID_RESULT => payload.push(json_to_value(message.result.clone())),
        _ => {}
    }

    pack_payload(&payload)
}

pub fn write_stream_item(message: &StreamItemMessage) -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[
        Value::from(MessageType::StreamItem as i32),
        headers_value(&message.headers),
        optional_string_value(message.invocation_id.as_deref()),
        json_to_value(Some(message.item.clone())),
    ])
}

pub fn write_stream_invocation(message: &StreamInvocationMessage) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = vec![
        Value::from(MessageType::StreamInvocation as i32),
        headers_value(&message.headers),
        Value::String(message.invocation_id.clone().into()),
        Value::String(message.target.clone().into()),
        json_array_value(&message.arguments),
    ];

    if let Some(stream_ids) = &message.stream_ids
        && !stream_ids.is_empty()
    {
        payload.push(Value::Array(
            stream_ids
                .iter()
                .map(|id| Value::String(id.clone().into()))
                .collect(),
        ));
    }

    pack_payload(&payload)
}

pub fn write_invocation(message: &InvocationMessage) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = vec![
        Value::from(MessageType::Invocation as i32),
        headers_value(&message.headers),
        optional_string_value(message.invocation_id.as_deref()),
        Value::String(message.target.clone().into()),
        json_array_value(&message.arguments),
    ];

    if let Some(stream_ids) = &message.stream_ids
        && !stream_ids.is_empty()
    {
        payload.push(Value::Array(
            stream_ids
                .iter()
                .map(|id| Value::String(id.clone().into()))
                .collect(),
        ));
    }

    pack_payload(&payload)
}

pub fn write_close() -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[Value::from(MessageType::Close as i32), Value::Nil])
}

pub fn write_ack(message: AckMessage) -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[
        Value::from(MessageType::Ack as i32),
        Value::from(message.sequence_id),
    ])
}

pub fn write_sequence(message: SequenceMessage) -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[
        Value::from(MessageType::Sequence as i32),
        Value::from(message.sequence_id),
    ])
}

fn pack_payload(payload: &[Value]) -> Result<Vec<u8>, ProtocolError> {
    let packed = encode_array(payload)?;
    Ok(BinaryMessageFormat::write(&packed))
}

fn encode_array(values: &[Value]) -> Result<Vec<u8>, ProtocolError> {
    let mut buffer = Vec::new();
    rmpv::encode::write_value(&mut buffer, &Value::Array(values.to_vec())).map_err(|error| {
        ProtocolError::MessagePack(format!("failed to encode messagepack payload: {error}"))
    })?;
    Ok(buffer)
}

fn headers_value(headers: &HashMap<String, String>) -> Value {
    if headers.is_empty() {
        Value::Map(vec![])
    } else {
        Value::Map(
            headers
                .iter()
                .map(|(key, value)| {
                    (
                        Value::String(key.clone().into()),
                        Value::String(value.clone().into()),
                    )
                })
                .collect(),
        )
    }
}

fn optional_string_value(value: Option<&str>) -> Value {
    value
        .map(|value| Value::String(value.to_string().into()))
        .unwrap_or(Value::Nil)
}

fn json_array_value(values: &[serde_json::Value]) -> Value {
    Value::Array(values.iter().map(|value| json_to_value(Some(value.clone()))).collect())
}

fn json_to_value(value: Option<serde_json::Value>) -> Value {
    match value {
        None | Some(serde_json::Value::Null) => Value::Nil,
        Some(serde_json::Value::Bool(value)) => Value::Boolean(value),
        Some(serde_json::Value::Number(number)) => {
            if let Some(value) = number.as_i64() {
                Value::Integer(value.into())
            } else if let Some(value) = number.as_u64() {
                Value::Integer(value.into())
            } else {
                Value::F64(number.as_f64().unwrap_or(0.0))
            }
        }
        Some(serde_json::Value::String(value)) => Value::String(value.into()),
        Some(serde_json::Value::Array(values)) => {
            Value::Array(values.into_iter().map(|value| json_to_value(Some(value))).collect())
        }
        Some(serde_json::Value::Object(values)) => Value::Map(
            values
                .into_iter()
                .map(|(key, value)| (Value::String(key.into()), json_to_value(Some(value))))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::message::PingMessage;

    #[test]
    fn write_ping_matches_dart_fixture() {
        assert_eq!(write_ping().unwrap(), vec![2, 145, 6]);
    }

    #[test]
    fn write_cancel_invocation_matches_dart_fixture() {
        let message = CancelInvocationMessage {
            headers: HashMap::new(),
            invocation_id: "1".into(),
        };
        assert_eq!(
            write_cancel_invocation(&message).unwrap(),
            vec![5, 147, 5, 128, 161, 49]
        );
    }

    #[test]
    fn write_completion_matches_dart_fixture() {
        let message = CompletionMessage {
            headers: HashMap::new(),
            invocation_id: "1".into(),
            error: None,
            result: None,
        };
        assert_eq!(
            write_completion(&message).unwrap(),
            vec![6, 148, 3, 128, 161, 49, 2]
        );
    }

    #[test]
    fn write_stream_item_matches_dart_fixture() {
        let message = StreamItemMessage {
            headers: HashMap::new(),
            invocation_id: Some("1".into()),
            item: serde_json::Value::String("item".into()),
        };
        assert_eq!(
            write_stream_item(&message).unwrap(),
            vec![10, 148, 2, 128, 161, 49, 164, 105, 116, 101, 109]
        );
    }

    #[test]
    fn write_stream_invocation_matches_dart_fixture() {
        let message = StreamInvocationMessage {
            headers: HashMap::new(),
            invocation_id: "1".into(),
            target: "target".into(),
            arguments: Vec::new(),
            stream_ids: None,
        };
        assert_eq!(
            write_stream_invocation(&message).unwrap(),
            vec![
                13, 149, 4, 128, 161, 49, 166, 116, 97, 114, 103, 101, 116, 144
            ]
        );
    }

    #[test]
    fn write_invocation_matches_dart_fixture() {
        let message = InvocationMessage {
            headers: HashMap::new(),
            invocation_id: None,
            target: "target".into(),
            arguments: Vec::new(),
            stream_ids: None,
        };
        assert_eq!(
            write_invocation(&message).unwrap(),
            vec![12, 149, 1, 128, 192, 166, 116, 97, 114, 103, 101, 116, 144]
        );
    }

    #[test]
    fn write_close_matches_dart_fixture() {
        assert_eq!(write_close().unwrap(), vec![3, 146, 7, 192]);
    }

    #[test]
    fn write_ack_matches_dart_fixture() {
        assert_eq!(write_ack(AckMessage { sequence_id: 1 }).unwrap(), vec![3, 146, 8, 1]);
    }

    #[test]
    fn write_sequence_matches_dart_fixture() {
        assert_eq!(
            write_sequence(SequenceMessage { sequence_id: 1 }).unwrap(),
            vec![3, 146, 9, 1]
        );
    }

    #[test]
    fn write_message_dispatches_ping() {
        assert_eq!(
            write_message(&HubMessage::Ping(PingMessage)).unwrap(),
            write_ping().unwrap()
        );
    }
}
