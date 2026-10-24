//! Parsing of synthetic `messagepack+brotli` frames, one per message type and compression
//! variant. The payloads are arbitrary; only the frame layout matters.
//!
//! Layout (compression flag, then payload): Invocation and StreamInvocation
//! `[type, headers, invocationId, target, isCompressed, arguments, streamIds?]`, StreamItem
//! `[2, headers, invocationId, isCompressed, item]`, Completion
//! `[3, headers, invocationId, resultKind, isCompressed?, result?]`.

#![cfg(feature = "messagepack-brotli")]
// `allow-unwrap-in-tests` does not cover helpers outside `#[test]`.
#![allow(clippy::unwrap_used)]

use std::io::Write;

use rmpv::Value;
use signalrr::{
    BinaryMessageFormat, HandshakeRequest, HubProtocol, HubProtocolInput, MessagePackBrotliProtocol,
};

fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, value).unwrap();
    bytes
}

fn frame(items: Vec<Value>) -> Vec<u8> {
    BinaryMessageFormat::write(&encode(&Value::Array(items)))
}

/// Payload as the server sends it with `isCompressed = 1`: msgpack `bin` with brotli-compressed
/// msgpack inside.
fn compressed(value: Value) -> Value {
    let mut output = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 11, 22);
        writer.write_all(&encode(&value)).unwrap();
    }
    Value::Binary(output)
}

fn arguments() -> Value {
    Value::Array(vec![
        Value::from("price"),
        Value::Map(vec![(Value::from(7), Value::from(1.25))]),
    ])
}

fn headers() -> Value {
    Value::Map(vec![])
}

fn parse(frame: &[u8]) -> String {
    match MessagePackBrotliProtocol::default().parse_messages(HubProtocolInput::Binary(frame)) {
        Ok(messages) => format!("{messages:#?}"),
        Err(error) => format!("error: {error}"),
    }
}

#[test]
fn invocation() {
    let cases = [
        ("compressed", Value::from(1), compressed(arguments())),
        ("uncompressed", Value::from(0), arguments()),
        ("nil flag", Value::Nil, arguments()),
    ];
    for (name, flag, payload) in cases {
        let frame = frame(vec![
            Value::from(1),
            headers(),
            Value::Nil,
            Value::from("update"),
            flag,
            payload,
        ]);
        insta::assert_snapshot!(format!("invocation {name}"), parse(&frame));
    }
}

#[test]
fn invocation_with_non_array_arguments_is_an_error() {
    for (name, flag, payload) in [
        ("compressed", Value::from(1), compressed(Value::from("x"))),
        ("uncompressed", Value::from(0), Value::from("x")),
    ] {
        let frame = frame(vec![
            Value::from(1),
            headers(),
            Value::Nil,
            Value::from("update"),
            flag,
            payload,
        ]);
        insta::assert_snapshot!(format!("invocation non-array {name}"), parse(&frame));
    }
}

#[test]
fn stream_invocation() {
    let frame = frame(vec![
        Value::from(4),
        headers(),
        Value::from("1"),
        Value::from("watch"),
        Value::from(1),
        compressed(arguments()),
        Value::Array(vec![]),
    ]);
    insta::assert_snapshot!(parse(&frame));
}

#[test]
fn stream_item() {
    for (name, flag, payload) in [
        ("compressed", Value::from(1), compressed(arguments())),
        ("uncompressed", Value::from(0), arguments()),
    ] {
        let frame = frame(vec![
            Value::from(2),
            headers(),
            Value::from("1"),
            flag,
            payload,
        ]);
        insta::assert_snapshot!(format!("stream item {name}"), parse(&frame));
    }
}

#[test]
fn completion() {
    let cases = [
        (
            "error",
            vec![
                Value::from(1),
                Value::from(1),
                compressed(Value::from("failed")),
            ],
        ),
        ("void", vec![Value::from(2)]),
        (
            "result compressed",
            vec![Value::from(3), Value::from(1), compressed(arguments())],
        ),
        (
            "result uncompressed",
            vec![Value::from(3), Value::from(0), Value::from(42)],
        ),
    ];
    for (name, tail) in cases {
        let mut items = vec![Value::from(3), headers(), Value::from("1")];
        items.extend(tail);
        insta::assert_snapshot!(format!("completion {name}"), parse(&frame(items)));
    }
}

#[test]
fn messages_without_payload() {
    let cases = [
        ("ping", vec![Value::from(6)]),
        (
            "close",
            vec![Value::from(7), Value::from("bye"), Value::from(true)],
        ),
        ("ack", vec![Value::from(8), Value::from(3)]),
        ("sequence", vec![Value::from(9), Value::from(4)]),
        (
            "cancel invocation is ignored",
            vec![Value::from(5), headers(), Value::from("1")],
        ),
        ("unknown type is ignored", vec![Value::from(99)]),
    ];
    for (name, items) in cases {
        insta::assert_snapshot!(name, parse(&frame(items)));
    }
}

#[test]
fn several_messages_in_one_frame() {
    let mut bytes = frame(vec![Value::from(6)]);
    bytes.extend(frame(vec![
        Value::from(1),
        headers(),
        Value::Nil,
        Value::from("update"),
        Value::from(1),
        compressed(arguments()),
    ]));
    insta::assert_snapshot!(parse(&bytes));
}

/// Stateful reconnect needs hub protocol version 2 in the handshake; the protocol reports it.
#[test]
fn handshake_version() {
    let protocol = MessagePackBrotliProtocol::default();
    let request = |stateful| HandshakeRequest {
        protocol: protocol.name().to_string(),
        version: signalrr::resolve_handshake_version(stateful, protocol.version(), None),
    };
    insta::assert_snapshot!(format!(
        "stateful: {:?}\nwithout stateful: {:?}",
        request(true),
        request(false)
    ));
}
