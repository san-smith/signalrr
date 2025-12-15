//! Rust client for ASP.NET Core SignalR.
//!
//! This crate provides a low-level implementation of the SignalR hub protocol,
//! with support for MessagePack serialization, streaming, and WebAssembly.
//!
//! # Quick start
//!
//! ```no_run
//! use signalrr::SignalRClient;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut client = SignalRClient::new("http://localhost:5000/chathub");
//!     client.start().await?;
//!     println!("Connected!");
//!     Ok(())
//! }
//! ```

mod connection;
mod error;
mod negotiate;

pub mod protocol;
pub use connection::SignalRClient;
pub use protocol::{Frame, MessagePackCodec, SignalRProtocolError};
