//! SignalR hub protocol messages and codecs.

use std::sync::Arc;

pub mod arguments;
pub mod json;
pub mod message;
#[cfg(feature = "messagepack")]
pub mod messagepack;
pub mod protocol;

pub use arguments::Arguments;
pub use json::JsonProtocol;
pub use message::{
    AckMessage, CancelInvocationMessage, CloseMessage, CompletionMessage, HubMessage,
    InvocationMessage, MessageType, PingMessage, SequenceMessage, StreamInvocationMessage,
    StreamItemMessage,
};
#[cfg(feature = "messagepack")]
pub use messagepack::MessagePackProtocol;
#[cfg(feature = "messagepack-brotli")]
pub use messagepack::brotli::{BrotliDecoder, DefaultBrotliDecoder, MessagePackBrotliProtocol};
pub use protocol::{HubProtocol, HubProtocolInput, ProtocolError};

/// Hub protocol used when none is configured: MessagePack with the `messagepack` feature,
/// JSON otherwise.
pub(crate) fn default_hub_protocol() -> Arc<dyn HubProtocol> {
    #[cfg(feature = "messagepack")]
    {
        Arc::new(MessagePackProtocol)
    }
    #[cfg(not(feature = "messagepack"))]
    {
        Arc::new(JsonProtocol)
    }
}
