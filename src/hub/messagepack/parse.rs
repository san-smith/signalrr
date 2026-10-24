//! MessagePack hub protocol parse helpers.

use std::collections::HashMap;

use rmpv::Value;

use crate::hub::arguments::Arguments;
use crate::hub::message::{
    AckMessage, CloseMessage, CompletionMessage, HubMessage, InvocationMessage, MessageType,
    PingMessage, SequenceMessage, StreamItemMessage,
};
use crate::hub::protocol::ProtocolError;

const ERROR_RESULT: i64 = 1;
const VOID_RESULT: i64 = 2;
const NON_VOID_RESULT: i64 = 3;

/// Deepest nesting accepted when skipping over encoded arguments.
const MAX_DEPTH: usize = 512;

/// Decodes the message array of one frame.
///
/// For an invocation the element at `arguments_index` stays encoded: it is returned as raw
/// bytes and `Nil` takes its place in the decoded items. This keeps large arguments out of
/// `rmpv::Value` until a handler asks for them.
pub(crate) fn decode_message(
    frame: &[u8],
    arguments_index: usize,
) -> Result<(Vec<Value>, Option<&[u8]>), ProtocolError> {
    let mut rest = frame;
    let len = rmp::decode::read_array_len(&mut rest)
        .map_err(|_| ProtocolError::InvalidPayload("invalid payload".into()))?;
    let mut items = Vec::new();
    let mut raw_arguments = None;
    for index in 0..len as usize {
        let is_invocation =
            items.first().and_then(Value::as_i64) == Some(MessageType::Invocation as i64);
        if index == arguments_index && is_invocation {
            let start = rest;
            skip_value(&mut rest, 0)?;
            raw_arguments = Some(&start[..start.len() - rest.len()]);
            items.push(Value::Nil);
        } else {
            items.push(rmpv::decode::read_value(&mut rest).map_err(|error| {
                ProtocolError::MessagePack(format!("failed to decode messagepack payload: {error}"))
            })?);
        }
    }
    Ok((items, raw_arguments))
}

/// Advances `rest` past one MessagePack value without decoding it.
fn skip_value(rest: &mut &[u8], depth: usize) -> Result<(), ProtocolError> {
    use rmp::Marker;

    if depth > MAX_DEPTH {
        return Err(ProtocolError::InvalidPayload(
            "messagepack nesting is too deep".into(),
        ));
    }
    let marker = rmp::decode::read_marker(rest).map_err(|_| truncated())?;
    let (bytes, values) = match marker {
        Marker::FixPos(_) | Marker::FixNeg(_) | Marker::Null | Marker::True | Marker::False => {
            (0, 0)
        }
        Marker::U8 | Marker::I8 => (1, 0),
        Marker::U16 | Marker::I16 => (2, 0),
        Marker::U32 | Marker::I32 | Marker::F32 => (4, 0),
        Marker::U64 | Marker::I64 | Marker::F64 => (8, 0),
        Marker::FixStr(len) => (usize::from(len), 0),
        Marker::Str8 | Marker::Bin8 => (read_len(rest, 1)?, 0),
        Marker::Str16 | Marker::Bin16 => (read_len(rest, 2)?, 0),
        Marker::Str32 | Marker::Bin32 => (read_len(rest, 4)?, 0),
        Marker::FixArray(len) => (0, usize::from(len)),
        Marker::Array16 => (0, read_len(rest, 2)?),
        Marker::Array32 => (0, read_len(rest, 4)?),
        Marker::FixMap(len) => (0, 2 * usize::from(len)),
        Marker::Map16 => (0, 2 * read_len(rest, 2)?),
        Marker::Map32 => (0, read_len(rest, 4)?.checked_mul(2).ok_or_else(truncated)?),
        Marker::FixExt1 => (2, 0),
        Marker::FixExt2 => (3, 0),
        Marker::FixExt4 => (5, 0),
        Marker::FixExt8 => (9, 0),
        Marker::FixExt16 => (17, 0),
        Marker::Ext8 => (read_len(rest, 1)? + 1, 0),
        Marker::Ext16 => (read_len(rest, 2)? + 1, 0),
        Marker::Ext32 => (read_len(rest, 4)? + 1, 0),
        Marker::Reserved => {
            return Err(ProtocolError::InvalidPayload(
                "reserved messagepack marker".into(),
            ));
        }
    };
    take(rest, bytes)?;
    for _ in 0..values {
        skip_value(rest, depth + 1)?;
    }
    Ok(())
}

fn take<'a>(rest: &mut &'a [u8], len: usize) -> Result<&'a [u8], ProtocolError> {
    if rest.len() < len {
        return Err(truncated());
    }
    let (head, tail) = rest.split_at(len);
    *rest = tail;
    Ok(head)
}

/// Reads a big-endian length of `size` bytes.
fn read_len(rest: &mut &[u8], size: usize) -> Result<usize, ProtocolError> {
    Ok(take(rest, size)?
        .iter()
        .fold(0, |len, byte| len << 8 | usize::from(*byte)))
}

fn truncated() -> ProtocolError {
    ProtocolError::InvalidPayload("truncated messagepack value".into())
}

/// Parses a decoded message; `raw_arguments` are the encoded invocation arguments from
/// [`decode_message`].
pub fn parse_message(
    data: &[Value],
    raw_arguments: Option<&[u8]>,
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
            HubMessage::Invocation(create_invocation_message(data, raw_arguments)?)
        }
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

pub(crate) fn create_message_headers(
    data: &[Value],
) -> Result<HashMap<String, String>, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::InvalidPayload("invalid headers".into()));
    }

    match &data[1] {
        Value::Nil => Ok(HashMap::new()),
        Value::Map(map) => map
            .iter()
            .map(|(key, value)| Ok((value_to_string(key)?, value_to_string(value)?)))
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

fn create_invocation_message(
    data: &[Value],
    raw_arguments: Option<&[u8]>,
) -> Result<InvocationMessage, ProtocolError> {
    let (true, Some(raw_arguments)) = (data.len() >= 5, raw_arguments) else {
        return Err(ProtocolError::InvalidPayload(
            "invalid payload for Invocation message".into(),
        ));
    };

    Ok(InvocationMessage {
        headers: create_message_headers(data)?,
        invocation_id: optional_string(&data[2])?,
        target: value_to_string(&data[3])?,
        arguments: Arguments::from_messagepack(raw_arguments.to_vec())?,
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

    /// Encoded empty MessagePack array.
    const EMPTY_ARRAY: u8 = 0x90;
    use crate::hub::message::PingMessage;

    #[test]
    fn create_message_headers_parses_empty_and_non_empty() {
        assert_eq!(
            create_message_headers(&[Value::from(1), Value::Nil]).unwrap(),
            HashMap::new()
        );
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
            parse_message(
                &[
                    Value::from(1),
                    Value::Map(vec![]),
                    Value::String("".into()),
                    Value::String("".into()),
                    Value::Nil,
                ],
                Some(&[EMPTY_ARRAY]),
            )
            .unwrap(),
            Some(HubMessage::Invocation(_))
        ));
        assert!(matches!(
            parse_message(&[Value::from(6)], None).unwrap(),
            Some(HubMessage::Ping(PingMessage))
        ));
        assert!(parse_message(&[Value::from(10)], None).unwrap().is_none());
    }
}
