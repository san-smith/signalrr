//! SignalR protocol implementation.
//!
//! This module provides types and codecs for the SignalR hub protocol,
//! with first-class support for MessagePack serialization.

mod codec;
mod frame;

pub use codec::{MessagePackCodec, SignalRProtocolError};
pub use frame::Frame;
