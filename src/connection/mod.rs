//! SignalR client connection management.
//!
//! This module handles the WebSocket connection lifecycle:
//! - Performing the initial handshake,
//! - Managing the WebSocket stream,
//! - Graceful shutdown.

pub mod bus;
pub mod manager;

use crate::connection::bus::MessageBus;
#[cfg(feature = "compression")]
use crate::transport::PayloadCodec;
use crate::{
    error::SignalRError,
    negotiate::negotiate,
    protocol::{Frame, MessagePackCodec},
};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tracing::{debug, info};

/// The state of a SignalR connection.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// No connection attempt has been made.
    Disconnected,
    /// Connection is in progress.
    Connecting,
    /// Connection is active.
    Connected,
    /// Connection lost, attempting to reconnect.
    Reconnecting { attempts: u32 },
    /// Connection is closed and will not reconnect.
    Closed,
}

/// A SignalR client connection.
///
/// This struct represents an active connection to a SignalR hub.
/// It handles the WebSocket stream and protocol handshake.
#[derive(Clone)]
pub struct Connection {
    ws_stream: Arc<RwLock<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
    bus: MessageBus,
    #[cfg(feature = "compression")]
    payload_codec: PayloadCodec,
}

impl Connection {
    /// Connects to the SignalR hub and performs the handshake.
    ///
    /// # Arguments
    ///
    /// * `hub_url` - The base URL of the SignalR hub (e.g., `http://localhost:5000/chathub`).
    ///
    /// # Returns
    ///
    /// * `Ok(Connection)` - An active connection ready to send/receive messages.
    /// * `Err(SignalRError)` - If connection or handshake fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use signalrr::connection::Connection;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let conn = Connection::connect("http://localhost:5000/chathub").await?;
    /// // Use the connection...
    /// # Ok(())
    /// # }
    ///
    pub async fn connect(
        hub_url: &str,
        #[cfg(feature = "compression")] payload_codec: PayloadCodec,
    ) -> Result<Self, SignalRError> {
        info!("Connecting to SignalR hub: {}", hub_url);
        let hub_url_parsed = url::Url::parse(hub_url)?;

        let connection_id = negotiate(&hub_url_parsed).await?;

        let ws_scheme = if hub_url_parsed.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        let host = hub_url_parsed.host_str().unwrap_or("localhost");
        let port = hub_url_parsed
            .port()
            .map_or(String::new(), |p| format!(":{}", p));
        let ws_url_str = format!("{}://{}{}{}", ws_scheme, host, port, hub_url_parsed.path());
        let mut ws_url = url::Url::parse(&ws_url_str)?;
        ws_url.query_pairs_mut().append_pair("id", &connection_id);

        debug!("WebSocket URL: {}", ws_url);

        let request = ws_url.as_str().into_client_request()?;
        let (mut ws_stream, _) = connect_async(request).await?;

        // Handshake
        let handshake = Frame::HandshakeRequest {
            protocol: "messagepack".to_string(),
            version: 1,
        };
        let payload = MessagePackCodec::encode(&handshake)?;
        ws_stream.send(Message::Binary(payload)).await?;

        let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(15));
        tokio::pin!(timeout);
        let mut handshake_complete = false;

        loop {
            tokio::select! {
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            let frame = MessagePackCodec::decode(&data)?;
                            match frame {
                                Frame::HandshakeResponse { error: None } => {
                                    info!("Handshake received");
                                    handshake_complete = true;
                                    break;
                                }
                                Frame::HandshakeResponse { error: Some(e) } => {
                                    return Err(SignalRError::HandshakeFailed(e));
                                }
                                _ => continue,
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            ws_stream.send(Message::Pong(payload)).await?;
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Text(_))) => {}
                        Some(Ok(Message::Frame(_))) => {}
                        Some(Ok(Message::Close(close_frame))) => {
                            let reason = close_frame.map(|f| f.reason.to_string()).unwrap_or_default();
                            return Err(SignalRError::HandshakeFailed(format!("Connection closed: {}", reason)));
                        }
                        Some(Err(e)) => return Err(SignalRError::WebSocket(e)),
                        None => return Err(SignalRError::HandshakeFailed("Connection closed".to_string())),
                    }
                }
                _ = &mut timeout => {
                    return Err(SignalRError::HandshakeFailed("Handshake timeout".to_string()));
                }
            }
        }

        if !handshake_complete {
            return Err(SignalRError::HandshakeFailed(
                "Handshake failed unexpectedly".to_string(),
            ));
        }

        // Wrap and start reader
        let ws_stream = Arc::new(RwLock::new(ws_stream));
        let (bus, _event_rx) = MessageBus::new();
        let conn = Self {
            ws_stream: ws_stream.clone(),
            bus,
            #[cfg(feature = "compression")]
            payload_codec,
        };
        conn.spawn_reader(ws_stream);
        Ok(conn)
    }

    fn spawn_reader(&self, ws_stream: Arc<RwLock<WebSocketStream<MaybeTlsStream<TcpStream>>>>) {
        let bus = self.bus.clone();
        #[cfg(feature = "compression")]
        let payload_codec = self.payload_codec.clone();

        tokio::spawn(async move {
            loop {
                let msg = {
                    let mut stream = ws_stream.write().await;
                    stream.next().await
                };
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        // decompression (if enabled)
                        #[cfg(feature = "compression")]
                        let payload_bytes = match payload_codec.decode(data) {
                            Ok(decompressed) => decompressed,
                            Err(e) => {
                                error!("Decompression failed: {}", e);
                                continue;
                            }
                        };
                        #[cfg(not(feature = "compression"))]
                        let payload_bytes = data;

                        if let Ok(frame) = MessagePackCodec::decode(&payload_bytes) {
                            match frame {
                                Frame::Invocation {
                                    invocation_id,
                                    target,
                                    arguments,
                                } => {
                                    debug!("Received Invocation: target={}", target);
                                    if invocation_id.is_none() {
                                        bus.dispatch_invocation(target, arguments).await;
                                    }
                                }
                                Frame::Completion {
                                    invocation_id,
                                    result,
                                    error,
                                } => {
                                    debug!("Received Completion: id={}", invocation_id);
                                    let res = if let Some(err) = error {
                                        Err(err)
                                    } else {
                                        Ok(result.unwrap_or(Value::Null))
                                    };
                                    bus.complete_invocation(invocation_id, res).await;
                                }
                                Frame::Ping => {
                                    // Pong already sent in handshake phase
                                }
                                Frame::StreamItem {
                                    invocation_id,
                                    item,
                                } => {
                                    bus.send_stream_item(invocation_id, Ok(item)).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    _ => {}
                }
            }
        });
    }

    pub async fn invoke<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        args: &[T],
    ) -> Result<R, SignalRError> {
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let json_args: Vec<Value> = args
            .iter()
            .map(|a| serde_json::to_value(a).unwrap())
            .collect();

        let rx = self.bus.register_pending(invocation_id.clone()).await?;

        let frame = Frame::Invocation {
            invocation_id: None,
            target: method.to_string(),
            arguments: json_args,
        };
        let payload = MessagePackCodec::encode(&frame)?;
        self.send_payload(payload).await?;

        let result = rx.await??;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn on<F>(&self, method: &str, handler: F)
    where
        F: Fn(Vec<Value>) + Send + Sync + 'static,
    {
        self.bus.register_handler(method.to_string(), handler).await;
    }

    /// Invokes a server method that returns a stream of values.
    ///
    /// # Arguments
    ///
    /// * `method` - The name of the streaming hub method.
    /// * `args` - Arguments to pass to the method.
    ///
    /// # Returns
    ///
    /// * `Ok(impl Stream<Item = Result<R, SignalRError>>)` - A stream of values.
    /// * `Err(SignalRError)` - If the invocation fails to start.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &signalrr::connection::Connection) -> Result<(), Box<dyn std::error::Error>> {
    /// use futures_util::StreamExt;
    /// let mut stream = conn.invoke_stream::<_, i32>("GetNumbers", &()).await?;
    /// while let Some(item) = stream.next().await {
    ///     println!("Number: {}", item?);
    /// }
    /// # Ok(())
    /// # }
    ///
    pub async fn invoke_stream<T: Serialize, R: for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        method: &str,
        args: &[T],
    ) -> Result<impl Stream<Item = Result<R, SignalRError>> + Unpin + Send, SignalRError> {
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let json_args: Vec<Value> = args
            .iter()
            .map(|a| serde_json::to_value(a).unwrap())
            .collect();

        let rx = self.bus.register_stream(invocation_id.clone()).await?;

        let frame = Frame::Invocation {
            invocation_id: Some(invocation_id),
            target: method.to_string(),
            arguments: json_args,
        };
        let payload = MessagePackCodec::encode(&frame)?;
        self.send_payload(payload).await?;

        let stream = rx.map(|res| {
            res.map_err(SignalRError::from)
                .and_then(|v| serde_json::from_value(v).map_err(SignalRError::from))
        });

        Ok(stream)
    }

    pub async fn shutdown(&mut self) -> Result<(), SignalRError> {
        info!("Shutting down SignalR connection");
        let close_frame = Frame::Close {
            error: None,
            allow_reconnect: None,
        };
        let payload = MessagePackCodec::encode(&close_frame)?;
        self.send_payload(payload).await?;
        self.ws_stream.write().await.close(None).await?;
        Ok(())
    }

    async fn send_payload(&self, payload: Vec<u8>) -> Result<(), SignalRError> {
        #[cfg(feature = "compression")]
        let final_payload = self.payload_codec.encode(payload)?;
        #[cfg(not(feature = "compression"))]
        let final_payload = payload;

        self.ws_stream
            .write()
            .await
            .send(Message::Binary(final_payload))
            .await?;
        Ok(())
    }
}

/// Main client for interacting with a SignalR hub.
pub struct SignalRClient {
    manager: manager::ConnectionManager,
}

impl SignalRClient {
    pub fn new(hub_url: &str) -> Self {
        Self {
            manager: manager::ConnectionManager::new(hub_url.to_string()),
        }
    }

    pub async fn start(&mut self) -> Result<(), SignalRError> {
        self.manager.start().await
    }

    /// Invokes a method on the server and waits for the result.
    ///
    /// # Arguments
    ///
    /// * `method` - The name of the hub method to call.
    /// * `args` - Arguments to pass to the method (must be serializable).
    ///
    /// # Returns
    ///
    /// * `Ok(R)` - The result returned by the server.
    /// * `Err(SignalRError)` - If the invocation fails or times out.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &signalrr::connection::Connection) -> Result<(), Box<dyn std::error::Error>> {
    /// let result: String = conn.invoke("GetUserName", &["user123"]).await?;
    /// println!("User name: {}", result);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn invoke<T: Serialize, R: for<'de> Deserialize<'de>>(
        &mut self,
        method: &str,
        args: &[T],
    ) -> Result<R, SignalRError> {
        self.manager.invoke(method, args).await
    }

    /// Registers a handler for server events.
    ///
    /// # Arguments
    ///
    /// * `method` - The name of the event (e.g., "NewsUpdate").
    /// * `handler` - A closure to call when the event is received.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &signalrr::connection::Connection) {
    /// conn.on("NewsUpdate", |args| {
    ///     if let Some(title) = args.get(0) {
    ///         println!("News: {}", title);
    ///     }
    /// }).await;
    /// # }
    /// ```
    pub async fn on<F>(&mut self, method: &str, handler: F)
    where
        F: Fn(Vec<Value>) + Send + Sync + 'static,
    {
        self.manager.on(method, handler).await.unwrap();
    }

    pub async fn shutdown(&mut self) -> Result<(), SignalRError> {
        self.manager.shutdown().await
    }

    /// Returns the current connection state.
    pub async fn state(&self) -> ConnectionState {
        self.manager.state().await
    }

    pub fn builder(hub_url: &str) -> SignalRClientBuilder {
        SignalRClientBuilder::new(hub_url)
    }
}

pub struct SignalRClientBuilder {
    hub_url: String,
    #[cfg(feature = "compression")]
    payload_codec: PayloadCodec,
}

impl SignalRClientBuilder {
    pub fn new(hub_url: &str) -> Self {
        Self {
            hub_url: hub_url.to_string(),
            #[cfg(feature = "compression")]
            payload_codec: PayloadCodec::default(),
        }
    }

    #[cfg(feature = "compression")]
    pub fn with_brotli_compression(mut self, quality: u32) -> Self {
        self.payload_codec = PayloadCodec::Brotli { quality };
        self
    }

    #[cfg(feature = "compression")]
    pub fn with_gzip_compression(mut self) -> Self {
        self.payload_codec = PayloadCodec::Gzip;
        self
    }

    pub async fn build(self) -> Result<SignalRClient, SignalRError> {
        let manager = manager::ConnectionManager::new(self.hub_url);
        #[cfg(feature = "compression")]
        manager.set_payload_codec(self.payload_codec);
        Ok(SignalRClient { manager })
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionState;

    #[test]
    fn test_connection_state_clone() {
        let state = ConnectionState::Reconnecting { attempts: 3 };
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }
}
