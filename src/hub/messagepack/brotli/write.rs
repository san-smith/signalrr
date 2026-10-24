//! MessagePack+Brotli hub protocol write helpers.

use rmpv::Value;

use crate::hub::message::{
    CompletionMessage, HubMessage, InvocationMessage, MessageType, StreamInvocationMessage,
    StreamItemMessage,
};
use crate::hub::messagepack::write::{
    headers_value, json_array_value, json_to_value, optional_string_value, pack_payload, write_ack,
    write_cancel_invocation, write_close, write_ping, write_sequence,
};
use crate::hub::protocol::ProtocolError;

const ERROR_RESULT: i64 = 1;
const VOID_RESULT: i64 = 2;
const NON_VOID_RESULT: i64 = 3;

pub fn write_message(message: &HubMessage) -> Result<Vec<u8>, ProtocolError> {
    match message {
        HubMessage::Invocation(message) => write_invocation_brotli(message),
        HubMessage::StreamInvocation(message) => write_stream_invocation_brotli(message),
        HubMessage::StreamItem(message) => write_stream_item_brotli(message),
        HubMessage::Completion(message) => write_completion_brotli(message),
        HubMessage::Ping(_) => write_ping(),
        HubMessage::CancelInvocation(message) => write_cancel_invocation(message),
        HubMessage::Close(_) => write_close(),
        HubMessage::Ack(message) => write_ack(*message),
        HubMessage::Sequence(message) => write_sequence(*message),
    }
}

pub fn write_invocation_brotli(message: &InvocationMessage) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = vec![
        Value::from(MessageType::Invocation as i32),
        headers_value(&message.headers),
        optional_string_value(message.invocation_id.as_deref()),
        Value::String(message.target.clone().into()),
        Value::from(0),
        message.arguments.messagepack_value()?,
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

pub fn write_stream_invocation_brotli(
    message: &StreamInvocationMessage,
) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = vec![
        Value::from(MessageType::StreamInvocation as i32),
        headers_value(&message.headers),
        Value::String(message.invocation_id.clone().into()),
        Value::String(message.target.clone().into()),
        Value::from(0),
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

pub fn write_stream_item_brotli(message: &StreamItemMessage) -> Result<Vec<u8>, ProtocolError> {
    pack_payload(&[
        Value::from(MessageType::StreamItem as i32),
        headers_value(&message.headers),
        optional_string_value(message.invocation_id.as_deref()),
        Value::from(0),
        json_to_value(Some(message.item.clone())),
    ])
}

pub fn write_completion_brotli(message: &CompletionMessage) -> Result<Vec<u8>, ProtocolError> {
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
        ERROR_RESULT => {
            payload.push(Value::from(0));
            payload.push(json_to_value(
                message
                    .error
                    .as_ref()
                    .map(|value| serde_json::Value::String(value.clone())),
            ));
        }
        NON_VOID_RESULT => {
            payload.push(Value::from(0));
            payload.push(json_to_value(message.result.clone()));
        }
        _ => {}
    }

    pack_payload(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn headers() -> HashMap<String, String> {
        HashMap::from([("header".to_string(), "value".to_string())])
    }

    #[test]
    fn write_invocation_brotli_matches_dart_fixture() {
        let message = InvocationMessage {
            headers: headers(),
            invocation_id: Some("invocationId".into()),
            target: "target".into(),
            arguments: vec![
                serde_json::Value::String("arg1".into()),
                serde_json::Value::String("arg2".into()),
            ]
            .into(),
            stream_ids: None,
        };

        assert_eq!(
            write_invocation_brotli(&message).unwrap(),
            vec![
                48, 150, 1, 129, 166, 104, 101, 97, 100, 101, 114, 165, 118, 97, 108, 117, 101,
                172, 105, 110, 118, 111, 99, 97, 116, 105, 111, 110, 73, 100, 166, 116, 97, 114,
                103, 101, 116, 0, 146, 164, 97, 114, 103, 49, 164, 97, 114, 103, 50
            ]
        );
    }

    #[test]
    fn write_stream_invocation_brotli_matches_dart_fixture() {
        let message = StreamInvocationMessage {
            headers: headers(),
            invocation_id: "invocationId".into(),
            target: "target".into(),
            arguments: vec![
                serde_json::Value::String("arg1".into()),
                serde_json::Value::String("arg2".into()),
            ],
            stream_ids: None,
        };

        assert_eq!(
            write_stream_invocation_brotli(&message).unwrap(),
            vec![
                48, 150, 4, 129, 166, 104, 101, 97, 100, 101, 114, 165, 118, 97, 108, 117, 101,
                172, 105, 110, 118, 111, 99, 97, 116, 105, 111, 110, 73, 100, 166, 116, 97, 114,
                103, 101, 116, 0, 146, 164, 97, 114, 103, 49, 164, 97, 114, 103, 50
            ]
        );
    }

    #[test]
    fn write_stream_item_brotli_matches_dart_fixture() {
        let message = StreamItemMessage {
            headers: headers(),
            invocation_id: Some("invocationId".into()),
            item: serde_json::Value::String("item".into()),
        };

        assert_eq!(
            write_stream_item_brotli(&message).unwrap(),
            vec![
                35, 149, 2, 129, 166, 104, 101, 97, 100, 101, 114, 165, 118, 97, 108, 117, 101,
                172, 105, 110, 118, 111, 99, 97, 116, 105, 111, 110, 73, 100, 0, 164, 105, 116,
                101, 109
            ]
        );
    }

    #[test]
    fn write_completion_brotli_encodes_error_void_and_non_void_results() {
        let error = CompletionMessage {
            headers: headers(),
            invocation_id: "invocationId".into(),
            error: Some("error".into()),
            result: None,
        };
        assert_eq!(
            write_completion_brotli(&error).unwrap(),
            vec![
                37, 150, 3, 129, 166, 104, 101, 97, 100, 101, 114, 165, 118, 97, 108, 117, 101,
                172, 105, 110, 118, 111, 99, 97, 116, 105, 111, 110, 73, 100, 1, 0, 165, 101, 114,
                114, 111, 114
            ]
        );

        let result = CompletionMessage {
            headers: headers(),
            invocation_id: "invocationId".into(),
            error: None,
            result: Some(serde_json::Value::String("result".into())),
        };
        let encoded = write_completion_brotli(&result).unwrap();
        assert_eq!(encoded[30], 3, "non-void completion must use resultKind 3");
        assert_eq!(encoded[31], 0, "uncompressed payload flag must be 0");
        assert!(
            encoded.ends_with(&[166, 114, 101, 115, 117, 108, 116]),
            "payload must contain msgpack string \"result\""
        );

        let void = CompletionMessage {
            headers: headers(),
            invocation_id: "invocationId".into(),
            error: None,
            result: None,
        };
        assert_eq!(
            write_completion_brotli(&void).unwrap(),
            vec![
                30, 148, 3, 129, 166, 104, 101, 97, 100, 101, 114, 165, 118, 97, 108, 117, 101,
                172, 105, 110, 118, 111, 99, 97, 116, 105, 111, 110, 73, 100, 2
            ]
        );
    }

    #[test]
    fn write_completion_brotli_round_trips_non_void_result() {
        use crate::hub::messagepack::brotli::decoder::DefaultBrotliDecoder;
        use crate::hub::messagepack::brotli::parse::parse_message;
        use std::sync::Arc;

        let message = CompletionMessage {
            headers: headers(),
            invocation_id: "invocationId".into(),
            error: None,
            result: Some(serde_json::Value::String("result".into())),
        };

        let payload = {
            let frame = write_completion_brotli(&message).unwrap();
            let frames = crate::framing::BinaryMessageFormat::parse(&frame).unwrap();
            rmpv::decode::read_value(&mut frames[0].as_slice()).unwrap()
        };

        let Value::Array(items) = payload else {
            panic!("expected array payload");
        };

        let decoder: Arc<dyn crate::hub::messagepack::brotli::BrotliDecoder> =
            Arc::new(DefaultBrotliDecoder);
        let decoded = parse_message(&items, None, &decoder)
            .unwrap()
            .expect("completion message");

        assert_eq!(decoded, HubMessage::Completion(message),);
    }
}
