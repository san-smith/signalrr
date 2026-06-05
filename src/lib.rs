//! Rust client for ASP.NET Core SignalR.
//!
//! # Quick start
//!
//! ```no_run
//! use signalrr::HubConnection;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), signalrr::Error> {
//!     let mut connection = HubConnection::builder("https://example.com/signalr")
//!         .with_access_token(|| async { Ok(String::new()) })
//!         .build();
//!
//!     connection.start().await?;
//!     println!("Connected: {:?}", connection.connection_id());
//!     connection.close().await?;
//!     Ok(())
//! }
//! ```

mod access_token;
mod bus;
mod connection;
mod error;
mod framing;
mod handshake;
mod hub;
mod negotiate;
mod transport;

pub mod builder;
pub mod hub_connection;

pub use access_token::{AccessTokenFactory, factory as access_token_factory};
pub use builder::HubConnectionBuilder;
pub use error::{Error, FramingError};
pub use framing::{BinaryMessageFormat, TextMessageFormat, RECORD_SEPARATOR};
pub use handshake::{
    HandshakeInput, HandshakeProtocol, HandshakeRequest, HandshakeResponse, HandshakeResult,
    RemainingData,
};
pub use hub::{
    AckMessage, CancelInvocationMessage, CloseMessage, CompletionMessage, HubMessage,
    HubProtocol, HubProtocolInput, InvocationMessage, JsonProtocol, MessagePackProtocol,
    MessageType, PingMessage, ProtocolError, SequenceMessage, StreamInvocationMessage,
    StreamItemMessage,
};
pub use hub_connection::{HubConnection, HubConnectionState};
pub use negotiate::{NegotiateOptions, NegotiateResponse, NegotiateResult, negotiate};
pub use transport::{TransferFormat, TransportMessage, WebSocketTransport};

#[doc(hidden)]
pub use bus::MessageBus;
