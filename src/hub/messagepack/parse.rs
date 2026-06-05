//! MessagePack hub protocol parse helpers.

use std::collections::HashMap;

use rmpv::Value;

use crate::hub::message::{
    AckMessage, CloseMessage, CompletionMessage, HubMessage, InvocationMessage, MessageType,
    PingMessage, SequenceMessage, StreamItemMessage,
};
use crate::hub::protocol::ProtocolError;

const ERROR_RESULT: i64 = 1;
const VOID_RESULT: i64 = 2;
const NON_VOID_RESULT: i64 = 3;

pub fn parse_message(data: &[Value]) -> Result<Option<HubMessage>, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::InvalidPayload("invalid payload".into()));
    }

    let message_type = value_to_i64(&data[0])?;
    let Some(message_type) = MessageType::from_i32(message_type as i32) else {
        return Ok(None);
    };

    let message = match message_type {
        MessageType::Invocation => HubMessage::Invocation(create_invocation_message(data)?),
        MessageType::StreamItem => HubMessage::StreamItem(create_stream_item_message(data)?),
        MessageType::Completion => HubMessage::Completion(create_completion_message(data)?),
        MessageType::Ping => HubMessage::Ping(create_ping_message(data)?),
        MessageType::Close => HubMessage::Close(create_close_message(data)?),
        MessageType::Ack => HubMessage::Ack(create_ack_message(data)?),
        MessageType::Sequence => HubMessage::Sequence(create_sequence_message(data)?),
        MessageType::StreamInvocation | MessageType::CancelInvocation => return Ok(None),
    };

    Ok(Some(message))
}

pub(crate) fn create_message_headers(data: &[Value]) -> Result<HashMap<String, String>, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidPayload("invalid headers".into()));
    }

    match &data[1] {
        Value::Nil => Ok(HashMap::new()),
        Value::Map(map) => map
            .iter()
            .map(|(key, value)| {
                Ok((
                    value_to_string(key)?,
                    value_to_string(value)?,
                ))
            })
            .collect(),
        _ => Err(ProtocolError::InvalidPayload("invalid headers".into())),
    }
}

pub(crate) fn create_close_message(data: &[Value]) -> Result<CloseMessage, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Close message".into(),
        ));
    }

    Ok(CloseMessage {
        error: optional_string(&data[1])?,
        allow_reconnect: data.get(2).and_then(|value| value.as_bool()),
    })
}

pub(crate) fn create_ping_message(data: &[Value]) -> Result<PingMessage, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Ping message".into(),
        ));
    }

    Ok(PingMessage)
}

fn create_completion_message(data: &[Value]) -> Result<CompletionMessage, ProtocolError> {
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
    if result_kind == ERROR_RESULT {
        return Ok(CompletionMessage {
            headers,
            invocation_id,
            error: optional_string(data.get(4).unwrap_or(&Value::Nil))?,
            result: None,
        });
    }

    if result_kind == NON_VOID_RESULT {
        return Ok(CompletionMessage {
            headers,
            invocation_id,
            error: None,
            result: data.get(4).map(value_to_json),
        });
    }

    Ok(CompletionMessage {
        headers,
        invocation_id,
        error: None,
        result: None,
    })
}

fn create_stream_item_message(data: &[Value]) -> Result<StreamItemMessage, ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for StreamItem message".into(),
        ));
    }

    Ok(StreamItemMessage {
        headers: create_message_headers(data)?,
        invocation_id: optional_string(&data[2])?,
        item: value_to_json(&data[3]),
    })
}

fn create_invocation_message(data: &[Value]) -> Result<InvocationMessage, ProtocolError> {
    if data.len() < 5 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Invocation message".into(),
        ));
    }

    Ok(InvocationMessage {
        headers: create_message_headers(data)?,
        invocation_id: optional_string(&data[2])?,
        target: value_to_string(&data[3])?,
        arguments: value_to_json_array(&data[4])?,
        stream_ids: Some(Vec::new()),
    })
}

pub(crate) fn create_ack_message(data: &[Value]) -> Result<AckMessage, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Ack message".into(),
        ));
    }

    Ok(AckMessage {
        sequence_id: value_to_i64(&data[1])?,
    })
}

pub(crate) fn create_sequence_message(data: &[Value]) -> Result<SequenceMessage, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Sequence message".into(),
        ));
    }

    Ok(SequenceMessage {
        sequence_id: value_to_i64(&data[1])?,
    })
}

pub(crate) fn value_to_i64(value: &Value) -> Result<i64, ProtocolError> {
    value
        .as_i64()
        .ok_or_else(|| ProtocolError::InvalidPayload("expected integer".into()))
}

pub(crate) fn value_to_string(value: &Value) -> Result<String, ProtocolError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ProtocolError::InvalidPayload("expected string".into()))
}

pub(crate) fn optional_string(value: &Value) -> Result<Option<String>, ProtocolError> {
    match value {
        Value::Nil => Ok(None),
        _ => value_to_string(value).map(Some),
    }
}

pub(crate) fn value_to_json(value: &Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

pub(crate) fn value_to_json_array(value: &Value) -> Result<Vec<serde_json::Value>, ProtocolError> {
    value
        .as_array()
        .map(|array| array.iter().map(value_to_json).collect())
        .ok_or_else(|| ProtocolError::InvalidPayload("expected array".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::message::PingMessage;

    #[test]
    fn create_message_headers_parses_empty_and_non_empty() {
        assert_eq!(create_message_headers(&[Value::from(1), Value::Nil]).unwrap(), HashMap::new());
        assert_eq!(
            create_message_headers(&[
                Value::from(1),
                Value::Map(vec![(
                    Value::String("hello".into()),
                    Value::String("world".into())
                )])
            ])
            .unwrap(),
            HashMap::from([("hello".to_string(), "world".to_string())])
        );
    }

    #[test]
    fn parse_message_handles_known_and_unknown_types() {
        assert!(matches!(
            parse_message(&[
                Value::from(1),
                Value::Map(vec![]),
                Value::String("".into()),
                Value::String("".into()),
                Value::Array(vec![]),
            ])
            .unwrap(),
            Some(HubMessage::Invocation(_))
        ));
        assert!(matches!(
            parse_message(&[Value::from(6)]).unwrap(),
            Some(HubMessage::Ping(PingMessage))
        ));
        assert!(parse_message(&[Value::from(10)]).unwrap().is_none());
    }
}
