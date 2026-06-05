//! Rust client for ASP.NET Core SignalR.
//!
//! # Quick start
//!
//! ```no_run
//! use signalrr::HubConnection;
//!
//! #[tokio::main]
//! async fn main() {
//!     let connection = HubConnection::builder("https://example.com/signalr")
//!         .build();
//!     assert_eq!(connection.url(), "https://example.com/signalr");
//! }
//! ```

mod bus;
mod error;
mod framing;
mod handshake;
mod negotiate;

pub mod builder;
pub mod hub_connection;

pub use builder::HubConnectionBuilder;
pub use error::{Error, FramingError};
pub use framing::{BinaryMessageFormat, TextMessageFormat, RECORD_SEPARATOR};
pub use handshake::{
    HandshakeInput, HandshakeProtocol, HandshakeRequest, HandshakeResponse, HandshakeResult,
    RemainingData,
};
pub use hub_connection::{HubConnection, HubConnectionState};
pub use negotiate::{NegotiateOptions, NegotiateResponse, negotiate};

#[doc(hidden)]
pub use bus::MessageBus;
