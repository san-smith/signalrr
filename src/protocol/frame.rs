//! SignalR protocol frames.
//!
//! This module defines the core [`Frame`] enum that represents all possible
//! message types in the ASP.NET Core SignalR protocol (v1).
//!
//! The protocol is transport-agnostic, but this implementation assumes
//! MessagePack serialization over WebSocket.
//!
//! For details, see the [official specification](https://github.com/dotnet/aspnetcore/blob/main/src/SignalR/docs/specs/HubProtocol.md).

use serde::{Deserialize, Serialize};

/// A SignalR protocol message frame.
///
/// Each frame corresponds to a specific message type defined in the SignalR protocol.
/// Frames are serialized to and from MessagePack binary format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Handshake request sent by the client to initiate the connection.
    ///
    /// Example (MessagePack): `[0, {"protocol": "messagepack", "version": 1}]`
    HandshakeRequest {
        /// Protocol name, e.g. `"messagepack"`.
        protocol: String,
        /// Protocol version (always `1` for current SignalR).
        version: u32,
    },

    /// Handshake response sent by the server to acknowledge the connection.
    ///
    /// Example (MessagePack): `[1, null, null]`
    HandshakeResponse {
        /// Optional error message if handshake failed.
        error: Option<String>,
    },

    /// Invocation from server to client (event or streaming item).
    ///
    /// Used for:
    /// - Server-to-client method calls (e.g. `OnNewsUpdate(...)`),
    /// - Streaming items in server-to-client streams.
    ///
    /// Example (MessagePack): `[1, null, "NewsUpdate", {"title": "Hello"}]`
    Invocation {
        /// Optional invocation ID (present only in streaming scenarios).
        invocation_id: Option<String>,
        /// Target method name on the client.
        target: String,
        /// Arguments passed to the method (serialized as JSON inside MessagePack).
        arguments: Vec<serde_json::Value>,
    },

    /// Сервер → клиент (элемент потока)
    StreamItem {
        invocation_id: String,
        item: serde_json::Value,
    },

    /// Completion message from server to client.
    ///
    /// Sent as a response to a client invocation, or to terminate a stream.
    ///
    /// Exactly one of `result` or `error` must be present.
    ///
    /// Example (success): `[3, "123", "ok", null]`
    /// Example (error):   `[3, "123", null, "Something failed"]`
    Completion {
        /// ID of the original client invocation.
        invocation_id: String,
        /// Result value (if successful).
        #[serde(default)]
        result: Option<serde_json::Value>,
        /// Error message (if failed).
        #[serde(default)]
        error: Option<String>,
    },

    /// Клиент → сервер (запрос на поток)
    StreamInvocation {
        invocation_id: String,
        target: String,
        arguments: Vec<serde_json::Value>,
        #[serde(default)]
        stream_ids: Vec<String>,
    },

    /// Клиент → сервер (отмена потока)
    CancelInvocation { invocation_id: String },

    /// Ping message for keep-alive.
    ///
    /// Can be sent by either side. No response is required.
    ///
    /// Example (MessagePack): `[6]`
    Ping,

    /// Close message to terminate the connection.
    ///
    /// Sent by either side to close the connection gracefully.
    ///
    /// Example (MessagePack): `[7, {"error": "Bye"}]`
    Close {
        /// Optional error message.
        #[serde(default)]
        error: Option<String>,
        /// Whether the client should attempt to reconnect.
        #[serde(default)]
        allow_reconnect: Option<bool>,
    },
}
