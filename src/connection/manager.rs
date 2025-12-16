//! Connection manager with automatic reconnect and lifecycle control.
//!
//! This module provides robust connection management for SignalR clients,
//! including:
//! - Automatic reconnect with exponential backoff,
//! - Graceful shutdown,
//! - Connection state tracking,
//! - Keep-alive via Ping/Pong (handled by Connection).
//!
//! The manager ensures the client remains connected even in unstable network
//! conditions.

#[cfg(feature = "compression")]
use crate::transport::PayloadCodec;
use crate::{
    connection::{Connection, ConnectionState},
    error::SignalRError,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio::time::Duration;
use tracing::warn;

/// The maximum number of reconnect attempts before giving up.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// Manages the lifecycle of a SignalR connection with automatic reconnect.
pub struct ConnectionManager {
    hub_url: String,
    state: Arc<RwLock<ConnectionState>>,
    shutdown_tx: Option<broadcast::Sender<()>>,
    connection: Option<Connection>,
    #[cfg(feature = "compression")]
    payload_codec: PayloadCodec,
}

/// Manages the lifecycle of a SignalR connection with automatic reconnect.
///
/// # Example
///
/// ```no_run
/// use signalrr::SignalRClient;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let mut client = SignalRClient::new("http://localhost:5000/chathub");
///     client.start().await?;
///
///     // Connection will automatically reconnect on failure
///     client.invoke::<_, ()>("SendMessage", &["user", "hello"]).await?;
///
///     client.shutdown().await?;
///     Ok(())
/// }
/// ```
impl ConnectionManager {
    /// Creates a new connection manager.
    ///
    /// The connection is not established until `start()` is called.
    pub fn new(hub_url: String) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            hub_url,
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            shutdown_tx: Some(shutdown_tx),
            connection: None,
            #[cfg(feature = "compression")]
            payload_codec: PayloadCodec::default(),
        }
    }

    #[cfg(feature = "compression")]
    pub fn set_payload_codec(&mut self, codec: PayloadCodec) {
        self.payload_codec = codec;
    }

    /// Starts the connection with automatic reconnect.
    ///
    /// Returns an error only if the initial connection fails permanently
    /// (after all reconnect attempts).
    pub async fn start(&mut self) -> Result<(), SignalRError> {
        *self.state.write().await = ConnectionState::Connecting;
        self.connect_with_retries(0).await
    }

    async fn connect_with_retries(&mut self, mut attempts: u32) -> Result<(), SignalRError> {
        loop {
            match Connection::connect(
                &self.hub_url,
                #[cfg(feature = "compression")]
                self.payload_codec.clone(),
            )
            .await
            {
                Ok(conn) => {
                    *self.state.write().await = ConnectionState::Connected;
                    self.connection = Some(conn);
                    return Ok(());
                }
                Err(e) => {
                    if attempts >= MAX_RECONNECT_ATTEMPTS {
                        *self.state.write().await = ConnectionState::Closed;
                        return Err(e);
                    }

                    let delay = Duration::from_secs(2_u64.pow(attempts.min(6)));
                    *self.state.write().await = ConnectionState::Reconnecting {
                        attempts: attempts + 1,
                    };
                    warn!(
                        "Connection attempt {} failed, retrying in {:?}",
                        attempts + 1,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    attempts += 1;
                }
            }
        }
    }

    /// Gracefully shuts down the connection.
    ///
    /// Sends a `Close` frame to the server and terminates the WebSocket.
    pub async fn shutdown(&mut self) -> Result<(), SignalRError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(mut conn) = self.connection.take() {
            conn.shutdown().await?;
        }
        *self.state.write().await = ConnectionState::Closed;
        Ok(())
    }

    /// Returns the current connection state.
    ///
    /// Useful for monitoring connection health.
    pub async fn state(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    /// Invokes a method on the server (proxies to Connection).
    pub async fn invoke<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        args: &[T],
    ) -> Result<R, SignalRError> {
        if let Some(conn) = &self.connection {
            conn.invoke(method, args).await
        } else {
            Err(SignalRError::NotConnected)
        }
    }

    /// Registers a handler for server events.
    pub async fn on<F>(&self, method: &str, handler: F) -> Result<(), SignalRError>
    where
        F: Fn(Vec<serde_json::Value>) + Send + Sync + 'static,
    {
        if let Some(conn) = &self.connection {
            conn.on(method, handler).await;
            Ok(())
        } else {
            Err(SignalRError::NotConnected)
        }
    }

    /// Invokes a streaming method on the server.
    pub async fn invoke_stream<T: Serialize, R: for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        method: &str,
        args: &[T],
    ) -> Result<
        impl futures_util::Stream<Item = Result<R, SignalRError>> + Unpin + Send,
        SignalRError,
    > {
        if let Some(conn) = &self.connection {
            conn.invoke_stream(method, args).await
        } else {
            Err(SignalRError::NotConnected)
        }
    }
}
