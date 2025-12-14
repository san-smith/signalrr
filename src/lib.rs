//! Rust client for ASP.NET Core SignalR.
//!
//! This crate provides a low-level implementation of the SignalR hub protocol,
//! with support for MessagePack serialization, streaming, and WebAssembly.
//!
//! # Quick start
//!
//! ```rust
//! use signalr::protocol::{Frame, MessagePackCodec};
//!
//! let frame = Frame::HandshakeRequest {
//!     protocol: "messagepack".into(),
//!     version: 1,
//! };
//! let bytes = MessagePackCodec::encode(&frame).unwrap();
//! let decoded = MessagePackCodec::decode(&bytes).unwrap();
//! ```
//!
//! For high-level client usage, see the [`SignalRClient`] builder (coming soon).

pub mod protocol;
pub use protocol::{Frame, MessagePackCodec, SignalRProtocolError};
