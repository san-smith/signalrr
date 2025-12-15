//! SignalR client connection management.
//!
//! This module handles the WebSocket connection lifecycle:
//! - Performing the initial handshake,
//! - Managing the WebSocket stream,
//! - Graceful shutdown.

use crate::{
    error::SignalRError,
    negotiate::negotiate,
    protocol::{Frame, MessagePackCodec},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// A SignalR client connection.
///
/// This struct represents an active connection to a SignalR hub.
/// It handles the WebSocket stream and protocol handshake.
pub struct Connection {
    // Пока храним только stream — для простоты
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
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
    /// ```
    pub async fn connect(hub_url: &str) -> Result<Self, SignalRError> {
        let hub_url_parsed = url::Url::parse(hub_url)?;

        // Получаем connection_id (может быть "local")
        let connection_id = negotiate(&hub_url_parsed).await?;

        let ws_scheme = if hub_url_parsed.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        let ws_url_str = format!(
            "{}://{}:{}{}",
            ws_scheme,
            hub_url_parsed.host_str().unwrap(),
            hub_url_parsed
                .port()
                .map_or(String::new(), |v| v.to_string()),
            hub_url_parsed.path()
        );
        let mut ws_url = url::Url::parse(&ws_url_str)?;
        ws_url.query_pairs_mut().append_pair("id", &connection_id);

        println!("WebSocket URL: {}", ws_url); // ← для отладки

        let request = ws_url.as_str().into_client_request()?;
        let (ws_stream, _) = connect_async(request).await?;
        let mut conn = Self { ws_stream };

        // Выполняем handshake
        conn.perform_handshake().await?;

        Ok(conn)
    }

    /// Performs the SignalR handshake.
    ///
    /// Sends a `HandshakeRequest` and waits for a `HandshakeResponse`.
    /// This method is called automatically by `Connection::connect`.
    async fn perform_handshake(&mut self) -> Result<(), SignalRError> {
        // Отправляем HandshakeRequest
        let handshake = Frame::HandshakeRequest {
            protocol: "messagepack".to_string(),
            version: 1,
        };
        let payload = MessagePackCodec::encode(&handshake)?;
        match self.ws_stream.send(Message::Binary(payload)).await {
            Ok(_) => println!("✅ Handshake sent"),
            Err(e) => println!("❌ Failed to send handshake: {}", e),
        }

        // Ждём HandshakeResponse с таймаутом
        let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(15));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                msg = self.ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            let frame = MessagePackCodec::decode(&data)?;
                            match frame {
                                Frame::HandshakeResponse { error: None } => {
                                    println!("✅ HandshakeResponse received");
                                    return Ok(())},
                                Frame::HandshakeResponse { error: Some(e) } => {
                                    return Err(SignalRError::HandshakeFailed(e));
                                }
                                _ => {
                                    // Игнорируем другие фреймы (Ping и т.д.)
                                    continue;
                                }
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                        // Необязательно, но можно отправить Pong
                        self.ws_stream.send(Message::Pong(payload)).await?;
                        continue;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Игнорируем Pong
                        continue;
                    }
                    Some(Ok(Message::Text(_))) => {
                        // SignalR не использует Text при MessagePack, но на всякий случай
                        continue;
                    }
                    Some(Ok(Message::Frame(_))) => {
                        // Игнорируем Frame (низкоуровневый)
                        continue;
                    }
                    Some(Ok(Message::Close(close_frame))) => {
                        let reason = close_frame.map(|f| f.reason.to_string()).unwrap_or_default();
                        return Err(SignalRError::HandshakeFailed(format!("Connection closed: {}", reason)));
                    }
                    Some(Err(e)) => {
                        return Err(SignalRError::WebSocket(e));
                    }
                    None => {
                        return Err(SignalRError::HandshakeFailed("Connection closed".to_string()));
                    }
                    }
                }
                _ = &mut timeout => {
                    return Err(SignalRError::HandshakeFailed("Handshake timeout".to_string()));
                }
            }
        }
    }
}

/// Main client for interacting with a SignalR hub.
pub struct SignalRClient {
    connection: Option<Connection>,
    hub_url: String,
}

impl SignalRClient {
    /// Creates a new client instance.
    pub fn new(hub_url: &str) -> Self {
        Self {
            connection: None,
            hub_url: hub_url.to_string(),
        }
    }

    /// Starts the connection to the hub.
    ///
    /// Performs negotiate, WebSocket upgrade, and handshake.
    pub async fn start(&mut self) -> Result<(), SignalRError> {
        let conn = Connection::connect(&self.hub_url).await?;
        self.connection = Some(conn);
        Ok(())
    }
}
