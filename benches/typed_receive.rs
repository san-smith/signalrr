//! Cost of turning a large server invocation into a typed value: frame → parsed message →
//! `T`. The payload is synthetic: a map with integer keys of items, each with nested maps
//! and arrays, about 6 MB of MessagePack.

use std::collections::HashMap;
use std::hint::black_box;
use std::io::Write;

use criterion::{Criterion, criterion_group, criterion_main};
use rmpv::Value;
use serde::Deserialize;
use signalrr::{
    Arguments, BinaryMessageFormat, HubMessage, HubProtocol, HubProtocolInput,
    MessagePackBrotliProtocol,
};

const ITEMS: i64 = 2_000;
const ENTRIES_PER_ITEM: i64 = 100;

#[derive(Deserialize)]
#[allow(dead_code)]
struct Update {
    name: String,
    full: bool,
    items: HashMap<i64, Item>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Item {
    id: i64,
    title: String,
    state: HashMap<String, f64>,
    tags: Vec<i64>,
    entries: HashMap<i64, Entry>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Entry {
    id: i64,
    key: i64,
    value: Option<i64>,
    open: bool,
}

fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Err(error) = rmpv::encode::write_value(&mut bytes, value) {
        panic!("encode: {error}");
    }
    bytes
}

fn arguments() -> Value {
    let entry = |id: i64| {
        Value::Map(vec![
            (Value::from("id"), Value::from(id)),
            (Value::from("key"), Value::from(id % 7)),
            (Value::from("value"), Value::from(id * 3)),
            (Value::from("open"), Value::from(true)),
        ])
    };
    let item = |id: i64| {
        Value::Map(vec![
            (Value::from("id"), Value::from(id)),
            (Value::from("title"), Value::from(format!("item {id}"))),
            (
                Value::from("state"),
                Value::Map(vec![
                    (Value::from("minute"), Value::from(77.0)),
                    (Value::from("second"), Value::from(4591.0)),
                ]),
            ),
            (
                Value::from("tags"),
                Value::Array((0..5).map(Value::from).collect()),
            ),
            (
                Value::from("entries"),
                Value::Map(
                    (0..ENTRIES_PER_ITEM)
                        .map(|n| {
                            let id = id * 1_000 + n;
                            (Value::from(id), entry(id))
                        })
                        .collect(),
                ),
            ),
        ])
    };
    let update = Value::Map(vec![
        (Value::from("name"), Value::from("all")),
        (Value::from("full"), Value::from(true)),
        (
            Value::from("items"),
            Value::Map((0..ITEMS).map(|id| (Value::from(id), item(id))).collect()),
        ),
    ]);
    Value::Array(vec![update])
}

/// `messagepack+brotli` invocation frame with the arguments compressed or as is.
fn frame(compress: bool) -> Vec<u8> {
    let arguments = arguments();
    let (flag, payload) = if compress {
        let mut output = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 5, 22);
            if let Err(error) = writer.write_all(&encode(&arguments)) {
                panic!("compress: {error}");
            }
        }
        (1, Value::Binary(output))
    } else {
        (0, arguments)
    };
    let message = Value::Array(vec![
        Value::from(1),
        Value::Map(vec![]),
        Value::Nil,
        Value::from("update"),
        Value::from(flag),
        payload,
    ]);
    BinaryMessageFormat::write(&encode(&message))
}

fn arguments_of(frame: &[u8]) -> Arguments {
    let protocol = MessagePackBrotliProtocol::default();
    let messages = match protocol.parse_messages(HubProtocolInput::Binary(frame)) {
        Ok(messages) => messages,
        Err(error) => panic!("parse: {error}"),
    };
    let Some(HubMessage::Invocation(invocation)) = messages.into_iter().next() else {
        panic!("expected an invocation");
    };
    invocation.arguments
}

/// What an `on` handler gets, JSON values, then `T`: the only path before typed receive.
fn via_json(frame: &[u8]) -> Update {
    let arguments = match arguments_of(frame).to_json() {
        Ok(arguments) => serde_json::Value::Array(arguments),
        Err(error) => panic!("to_json: {error}"),
    };
    match serde_json::from_value::<(Update,)>(arguments) {
        Ok((update,)) => update,
        Err(error) => panic!("deserialize: {error}"),
    }
}

/// What an `on_typed` handler gets.
fn typed(frame: &[u8]) -> Update {
    match arguments_of(frame).deserialize::<(Update,)>() {
        Ok((update,)) => update,
        Err(error) => panic!("deserialize: {error}"),
    }
}

fn typed_receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("typed_receive");
    group.sample_size(20);
    for (name, compress) in [("uncompressed", false), ("brotli", true)] {
        let frame = frame(compress);
        group.bench_function(format!("via_json/{name}"), |b| {
            b.iter(|| via_json(black_box(&frame)))
        });
        group.bench_function(format!("typed/{name}"), |b| {
            b.iter(|| typed(black_box(&frame)))
        });
    }
    group.finish();
}

criterion_group!(benches, typed_receive);
criterion_main!(benches);
