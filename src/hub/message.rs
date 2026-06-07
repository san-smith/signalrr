//! SignalR hub message types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Numeric hub message type identifiers from the SignalR protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum MessageType {
    Invocation = 1,
    StreamItem = 2,
    Completion = 3,
    StreamInvocation = 4,
    CancelInvocation = 5,
    Ping = 6,
    Close = 7,
    Ack = 8,
    Sequence = 9,
}

impl MessageType {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Invocation),
            2 => Some(Self::StreamItem),
            3 => Some(Self::Completion),
            4 => Some(Self::StreamInvocation),
            5 => Some(Self::CancelInvocation),
            6 => Some(Self::Ping),
            7 => Some(Self::Close),
            8 => Some(Self::Ack),
            9 => Some(Self::Sequence),
            _ => None,
        }
    }
}

/// All supported hub protocol messages.
#[derive(Debug, Clone, PartialEq)]
pub enum HubMessage {
    Invocation(InvocationMessage),
    StreamInvocation(StreamInvocationMessage),
    StreamItem(StreamItemMessage),
    Completion(CompletionMessage),
    CancelInvocation(CancelInvocationMessage),
    Ping(PingMessage),
    Close(CloseMessage),
    Ack(AckMessage),
    Sequence(SequenceMessage),
}

impl HubMessage {
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::Invocation(_) => MessageType::Invocation,
            Self::StreamInvocation(_) => MessageType::StreamInvocation,
            Self::StreamItem(_) => MessageType::StreamItem,
            Self::Completion(_) => MessageType::Completion,
            Self::CancelInvocation(_) => MessageType::CancelInvocation,
            Self::Ping(_) => MessageType::Ping,
            Self::Close(_) => MessageType::Close,
            Self::Ack(_) => MessageType::Ack,
            Self::Sequence(_) => MessageType::Sequence,
        }
    }

    pub fn from_json_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let message_type = MessageType::from_i32(obj.get("type")?.as_i64()? as i32)?;

        match message_type {
            MessageType::Invocation => Some(Self::Invocation(InvocationMessage::from_json(obj)?)),
            MessageType::StreamInvocation => Some(Self::StreamInvocation(
                StreamInvocationMessage::from_json(obj)?,
            )),
            MessageType::StreamItem => Some(Self::StreamItem(StreamItemMessage::from_json(obj)?)),
            MessageType::Completion => Some(Self::Completion(CompletionMessage::from_json(obj)?)),
            MessageType::CancelInvocation => Some(Self::CancelInvocation(
                CancelInvocationMessage::from_json(obj)?,
            )),
            MessageType::Ping => Some(Self::Ping(PingMessage)),
            MessageType::Close => Some(Self::Close(CloseMessage::from_json(obj)?)),
            MessageType::Ack => Some(Self::Ack(AckMessage::from_json(obj)?)),
            MessageType::Sequence => Some(Self::Sequence(SequenceMessage::from_json(obj)?)),
        }
    }

    pub fn to_json_value(&self) -> Value {
        match self {
            Self::Invocation(message) => message.to_json(),
            Self::StreamInvocation(message) => message.to_json(),
            Self::StreamItem(message) => message.to_json(),
            Self::Completion(message) => message.to_json(),
            Self::CancelInvocation(message) => message.to_json(),
            Self::Ping(_) => serde_json::json!({ "type": MessageType::Ping as i32 }),
            Self::Close(message) => message.to_json(),
            Self::Ack(message) => message.to_json(),
            Self::Sequence(message) => message.to_json(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvocationMessage {
    pub headers: HashMap<String, String>,
    pub invocation_id: Option<String>,
    pub target: String,
    pub arguments: Vec<Value>,
    pub stream_ids: Option<Vec<String>>,
}

impl InvocationMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            headers: parse_headers(obj.get("headers").unwrap_or(&Value::Null)),
            invocation_id: parse_optional_string(obj.get("invocationId")),
            target: obj.get("target")?.as_str()?.to_string(),
            arguments: obj.get("arguments")?.as_array()?.to_vec(),
            stream_ids: parse_optional_string_list(obj.get("streamIds")),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::Invocation as i32).into()),
        );
        insert_optional_map(&mut map, "headers", &self.headers);
        insert_optional_string(&mut map, "invocationId", self.invocation_id.as_deref());
        map.insert("target".into(), Value::String(self.target.clone()));
        map.insert("arguments".into(), Value::Array(self.arguments.clone()));
        insert_optional_string_list(&mut map, "streamIds", self.stream_ids.as_deref());
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInvocationMessage {
    pub headers: HashMap<String, String>,
    pub invocation_id: String,
    pub target: String,
    pub arguments: Vec<Value>,
    pub stream_ids: Option<Vec<String>>,
}

impl StreamInvocationMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            headers: parse_headers(obj.get("headers").unwrap_or(&Value::Null)),
            invocation_id: obj.get("invocationId")?.as_str()?.to_string(),
            target: obj.get("target")?.as_str()?.to_string(),
            arguments: obj.get("arguments")?.as_array()?.to_vec(),
            stream_ids: parse_optional_string_list(obj.get("streamIds")),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::StreamInvocation as i32).into()),
        );
        insert_optional_map(&mut map, "headers", &self.headers);
        map.insert(
            "invocationId".into(),
            Value::String(self.invocation_id.clone()),
        );
        map.insert("target".into(), Value::String(self.target.clone()));
        map.insert("arguments".into(), Value::Array(self.arguments.clone()));
        insert_optional_string_list(&mut map, "streamIds", self.stream_ids.as_deref());
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamItemMessage {
    pub headers: HashMap<String, String>,
    pub invocation_id: Option<String>,
    pub item: Value,
}

impl StreamItemMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            headers: parse_headers(obj.get("headers").unwrap_or(&Value::Null)),
            invocation_id: parse_optional_string(obj.get("invocationId")),
            item: obj.get("item")?.clone(),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::StreamItem as i32).into()),
        );
        insert_optional_map(&mut map, "headers", &self.headers);
        insert_optional_string(&mut map, "invocationId", self.invocation_id.as_deref());
        map.insert("item".into(), self.item.clone());
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionMessage {
    pub headers: HashMap<String, String>,
    pub invocation_id: String,
    pub error: Option<String>,
    pub result: Option<Value>,
}

impl CompletionMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            headers: parse_headers(obj.get("headers").unwrap_or(&Value::Null)),
            invocation_id: obj.get("invocationId")?.as_str()?.to_string(),
            error: parse_optional_string(obj.get("error")),
            result: obj.get("result").cloned(),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::Completion as i32).into()),
        );
        insert_optional_map(&mut map, "headers", &self.headers);
        map.insert(
            "invocationId".into(),
            Value::String(self.invocation_id.clone()),
        );
        insert_optional_string(&mut map, "error", self.error.as_deref());
        if let Some(result) = &self.result {
            map.insert("result".into(), result.clone());
        }
        Value::Object(map)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CancelInvocationMessage {
    pub headers: HashMap<String, String>,
    pub invocation_id: String,
}

impl CancelInvocationMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            headers: parse_headers(obj.get("headers").unwrap_or(&Value::Null)),
            invocation_id: obj.get("invocationId")?.as_str()?.to_string(),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::CancelInvocation as i32).into()),
        );
        insert_optional_map(&mut map, "headers", &self.headers);
        map.insert(
            "invocationId".into(),
            Value::String(self.invocation_id.clone()),
        );
        Value::Object(map)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PingMessage;

#[derive(Debug, Clone, PartialEq)]
pub struct CloseMessage {
    pub error: Option<String>,
    pub allow_reconnect: Option<bool>,
}

impl CloseMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            error: parse_optional_string(obj.get("error")),
            allow_reconnect: obj.get("allowReconnect").and_then(|value| value.as_bool()),
        })
    }

    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::Number((MessageType::Close as i32).into()),
        );
        insert_optional_string(&mut map, "error", self.error.as_deref());
        if let Some(allow_reconnect) = self.allow_reconnect {
            map.insert("allowReconnect".into(), Value::Bool(allow_reconnect));
        }
        Value::Object(map)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckMessage {
    pub sequence_id: i64,
}

impl AckMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            sequence_id: obj.get("sequenceId")?.as_i64()?,
        })
    }

    #[allow(clippy::wrong_self_convention)]
    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": MessageType::Ack as i32,
            "sequenceId": self.sequence_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceMessage {
    pub sequence_id: i64,
}

impl SequenceMessage {
    fn from_json(obj: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self {
            sequence_id: obj.get("sequenceId")?.as_i64()?,
        })
    }

    #[allow(clippy::wrong_self_convention)]
    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": MessageType::Sequence as i32,
            "sequenceId": self.sequence_id,
        })
    }
}

fn parse_headers(value: &Value) -> HashMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|string| (key.clone(), string.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| value.as_str().map(str::to_string))
}

fn parse_optional_string_list(value: Option<&Value>) -> Option<Vec<String>> {
    value.map(|value| {
        value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    })
}

fn insert_optional_map(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: &HashMap<String, String>,
) {
    if !value.is_empty() {
        map.insert(
            key.into(),
            Value::Object(
                value
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
}

fn insert_optional_string(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        map.insert(key.into(), Value::String(value.to_string()));
    }
}

fn insert_optional_string_list(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&[String]>,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        map.insert(
            key.into(),
            Value::Array(value.iter().cloned().map(Value::String).collect()),
        );
    }
}
