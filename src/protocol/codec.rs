//! MessagePack codec for SignalR frames.
//!
//! Provides serialization (`encode`) and deserialization (`decode`) of [`Frame`]
//! to/from MessagePack binary format, compatible with ASP.NET Core SignalR.

use serde_json::Value;

use crate::protocol::Frame;
use std::io::Cursor;

/// Errors that can occur during protocol encoding or decoding.
#[derive(thiserror::Error, Debug)]
pub enum SignalRProtocolError {
    /// Failed to encode a frame to MessagePack.
    #[error("MessagePack encode error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    /// Failed to decode MessagePack data into a frame.
    #[error("MessagePack decode error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),

    /// Received a message that does not conform to the SignalR protocol.
    #[error("Invalid handshake response format")]
    InvalidHandshakeResponse,

    /// Received a message type not recognized by this implementation.
    #[error("Unexpected message type: {0}")]
    UnexpectedMessageType(u32),
}

/// Codec for SignalR MessagePack protocol.
///
/// This struct provides static methods to convert between [`Frame`] and MessagePack bytes.
pub struct MessagePackCodec;

impl MessagePackCodec {
    /// Encodes a [`Frame`] into MessagePack binary format.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails or if the frame type is not yet implemented.
    pub fn encode(frame: &Frame) -> Result<Vec<u8>, SignalRProtocolError> {
        let mut buf = Vec::new();

        match frame {
            Frame::HandshakeRequest { protocol, version } => {
                // [0, {"protocol": "messagepack", "version": 1}]
                let handshake = (
                    0u32,
                    serde_json::json!({
                        "protocol": protocol,
                        "version": version
                    }),
                );
                rmp_serde::encode::write(&mut buf, &handshake)?;
            }
            Frame::Invocation {
                invocation_id,
                target,
                arguments,
            } => {
                // Формат: [1, invocationId, target, arguments...]
                let mut payload = vec![
                    serde_json::Value::Number(1.into()),
                    if let Some(id) = invocation_id {
                        serde_json::Value::String(id.clone())
                    } else {
                        serde_json::Value::Null
                    },
                    serde_json::Value::String(target.clone()),
                ];
                payload.extend(arguments.iter().cloned());
                rmp_serde::encode::write(&mut buf, &payload)?;
            }
            Frame::Completion {
                invocation_id,
                result,
                error,
            } => {
                // Формат: [3, invocationId, result, error]
                // result и error не могут быть одновременно Some
                let result_val = result.clone().unwrap_or(serde_json::Value::Null);
                let error_val = if let Some(e) = error {
                    serde_json::Value::String(e.clone())
                } else {
                    serde_json::Value::Null
                };
                rmp_serde::encode::write(
                    &mut buf,
                    &(3u32, invocation_id.clone(), result_val, error_val),
                )?;
            }
            Frame::Ping => {
                // [6]
                rmp_serde::encode::write(&mut buf, &(6u32,))?;
            }
            Frame::Close {
                error,
                allow_reconnect,
            } => {
                // [7, {"error": "...", "allowReconnect": true}]
                let mut map = serde_json::Map::new();
                if let Some(e) = error {
                    map.insert("error".to_string(), serde_json::Value::String(e.clone()));
                }
                if let Some(r) = allow_reconnect {
                    map.insert("allowReconnect".to_string(), serde_json::Value::Bool(*r));
                }
                let close_payload = if map.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Object(map)
                };
                rmp_serde::encode::write(&mut buf, &(7u32, close_payload))?;
            }
            _ => {
                // Остальные фреймы — реализуем позже
                unimplemented!("Encoding for this frame type is not implemented yet");
            }
        }

        Ok(buf)
    }

    /// Decodes MessagePack binary data into a [`Frame`].
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid MessagePack or does not conform to the SignalR protocol.
    pub fn decode(data: &[u8]) -> Result<Frame, SignalRProtocolError> {
        // Десериализуем весь фрейм как массив JSON-значений
        let array: Vec<Value> = rmp_serde::from_read(Cursor::new(data))?;

        if array.is_empty() {
            return Err(SignalRProtocolError::InvalidHandshakeResponse);
        }

        // Первый элемент — всегда тип сообщения (integer)
        let message_type = array[0]
            .as_u64()
            .ok_or(SignalRProtocolError::InvalidHandshakeResponse)?
            as u32;

        match message_type {
            1 => {
                // Сначала проверяем HandshakeResponse: [1, null, null]
                if array.len() == 3 && array[1].is_null() && array[2].is_null() {
                    return Ok(Frame::HandshakeResponse { error: None });
                }

                // Иначе — это Invocation: [1, invocationId, target, args...]
                if array.len() < 3 {
                    return Err(SignalRProtocolError::InvalidHandshakeResponse);
                }

                let invocation_id = match &array[1] {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Null => None,
                    _ => return Err(SignalRProtocolError::InvalidHandshakeResponse),
                };

                let target = match &array[2] {
                    serde_json::Value::String(s) => s.clone(),
                    _ => return Err(SignalRProtocolError::InvalidHandshakeResponse),
                };

                let arguments = array.into_iter().skip(3).collect();
                Ok(Frame::Invocation {
                    invocation_id,
                    target,
                    arguments,
                })
            }
            3 => {
                if array.len() != 4 {
                    return Err(SignalRProtocolError::InvalidHandshakeResponse);
                }
                let invocation_id = match &array[1] {
                    serde_json::Value::String(s) => s.clone(),
                    _ => return Err(SignalRProtocolError::InvalidHandshakeResponse),
                };
                let result = if array[2].is_null() {
                    None
                } else {
                    Some(array[2].clone())
                };
                let error = match &array[3] {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Null => None,
                    _ => return Err(SignalRProtocolError::InvalidHandshakeResponse),
                };
                return Ok(Frame::Completion {
                    invocation_id,
                    result,
                    error,
                });
            }
            6 => {
                // Ping: [6]
                if array.len() == 1 {
                    return Ok(Frame::Ping);
                }
                return Err(SignalRProtocolError::InvalidHandshakeResponse);
            }
            7 => {
                // Close: [7, {"error": "...", "allowReconnect": true}]
                if array.len() != 2 {
                    return Err(SignalRProtocolError::InvalidHandshakeResponse);
                }

                let error = if let Some(obj) = array[1].as_object() {
                    obj.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                };

                let allow_reconnect = if let Some(obj) = array[1].as_object() {
                    obj.get("allowReconnect").and_then(|v| v.as_bool())
                } else {
                    None
                };

                return Ok(Frame::Close {
                    error,
                    allow_reconnect,
                });
            }
            _ => {
                return Err(SignalRProtocolError::UnexpectedMessageType(message_type));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_handshake_request() {
        let frame = Frame::HandshakeRequest {
            protocol: "messagepack".into(),
            version: 1,
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();

        // Проверим, что это корректный MessagePack-массив:
        // [0, {"protocol": "messagepack", "version": 1}]
        let decoded_as_json: Vec<serde_json::Value> = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded_as_json.len(), 2);
        assert_eq!(decoded_as_json[0], serde_json::Value::Number(0.into()));

        if let serde_json::Value::Object(map) = &decoded_as_json[1] {
            assert_eq!(
                map.get("protocol"),
                Some(&serde_json::Value::String("messagepack".to_string()))
            );
            assert_eq!(
                map.get("version"),
                Some(&serde_json::Value::Number(1.into()))
            );
        } else {
            panic!("Expected object in handshake payload");
        }
    }

    #[test]
    fn test_handshake_response() {
        // [1, null, null] в MessagePack = 0x93 0x01 0xc0 0xc0
        let data = vec![0x93, 0x01, 0xc0, 0xc0];
        let frame = MessagePackCodec::decode(&data).unwrap();
        assert!(matches!(frame, Frame::HandshakeResponse { error: None }));
    }

    #[test]
    fn test_ping() {
        let frame = Frame::Ping;
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        // MessagePack: [6]
        assert_eq!(encoded, vec![0x91, 0x06]);
        let decoded = MessagePackCodec::decode(&encoded).unwrap();
        assert!(matches!(decoded, Frame::Ping));
    }

    #[test]
    fn test_close() {
        let frame = Frame::Close {
            error: Some("test error".into()),
            allow_reconnect: None,
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        let decoded = MessagePackCodec::decode(&encoded).unwrap();

        match decoded {
            Frame::Close { error, .. } => {
                assert_eq!(error, Some("test error".to_string()));
            }
            _ => panic!("Expected Close frame"),
        }
    }

    #[test]
    fn test_invocation_without_id() {
        // [1, null, "NewsUpdate", {"title": "Hello"}]
        let frame = Frame::Invocation {
            invocation_id: None,
            target: "NewsUpdate".to_string(),
            arguments: vec![serde_json::json!({"title": "Hello"})],
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        let decoded = MessagePackCodec::decode(&encoded).unwrap();

        match decoded {
            Frame::Invocation {
                invocation_id,
                target,
                arguments,
            } => {
                assert!(invocation_id.is_none());
                assert_eq!(target, "NewsUpdate");
                assert_eq!(arguments.len(), 1);
                assert_eq!(arguments[0], serde_json::json!({"title": "Hello"}));
            }
            _ => panic!("Expected Invocation"),
        }
    }

    #[test]
    fn test_invocation_with_id() {
        // [1, "abc123", "StreamItem", 42]
        let frame = Frame::Invocation {
            invocation_id: Some("abc123".to_string()),
            target: "StreamItem".to_string(),
            arguments: vec![serde_json::json!(42)],
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        let decoded = MessagePackCodec::decode(&encoded).unwrap();

        match decoded {
            Frame::Invocation {
                invocation_id,
                target,
                arguments,
            } => {
                assert_eq!(invocation_id, Some("abc123".to_string()));
                assert_eq!(target, "StreamItem");
                assert_eq!(arguments[0], serde_json::json!(42));
            }
            _ => panic!("Expected Invocation"),
        }
    }

    #[test]
    fn test_completion_success() {
        // [3, "xyz789", "result value", null]
        let frame = Frame::Completion {
            invocation_id: "xyz789".to_string(),
            result: Some(serde_json::json!("result value")),
            error: None,
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        let decoded = MessagePackCodec::decode(&encoded).unwrap();

        match decoded {
            Frame::Completion {
                invocation_id,
                result,
                error,
            } => {
                assert_eq!(invocation_id, "xyz789");
                assert_eq!(result, Some(serde_json::json!("result value")));
                assert!(error.is_none());
            }
            _ => panic!("Expected Completion"),
        }
    }

    #[test]
    fn test_completion_error() {
        // [3, "err123", null, "Something went wrong"]
        let frame = Frame::Completion {
            invocation_id: "err123".to_string(),
            result: None,
            error: Some("Something went wrong".to_string()),
        };
        let encoded = MessagePackCodec::encode(&frame).unwrap();
        let decoded = MessagePackCodec::decode(&encoded).unwrap();

        match decoded {
            Frame::Completion {
                invocation_id,
                result,
                error,
            } => {
                assert_eq!(invocation_id, "err123");
                assert!(result.is_none());
                assert_eq!(error, Some("Something went wrong".to_string()));
            }
            _ => panic!("Expected Completion"),
        }
    }
}
