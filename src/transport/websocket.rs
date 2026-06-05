//! WebSocket transport for SignalR.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tracing::{debug, error, info};

use crate::error::Error;

/// A message received from the WebSocket transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMessage {
    Text(String),
    Binary(Vec<u8>),
    Closed(Option<String>),
}

/// WebSocket transport handle.
pub struct WebSocketTransport {
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    read_task: tokio::task::JoinHandle<()>,
}

impl WebSocketTransport {
    /// Opens a WebSocket connection to `url`.
    ///
    /// When `access_token` is provided, it is appended as the `access_token` query parameter.
    pub async fn connect(
        url: &str,
        access_token: Option<&str>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<TransportMessage>), Error> {
        let target_url = super::append_access_token(url, access_token)?;
        debug!(url = %target_url, "connecting websocket");

        let request = target_url
            .as_str()
            .into_client_request()
            .map_err(|error| Error::WebSocket(Box::new(error)))?;
        let (ws_stream, _) = connect_async(request).await.map_err(|error| {
            error!(?error, "websocket connection failed");
            Error::WebSocket(Box::new(error))
        })?;
        info!(url = %target_url, "websocket connected");

        let (write, read) = ws_stream.split();
        let sender = Arc::new(Mutex::new(write));
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();

        let read_task = tokio::spawn(read_loop(read, incoming_tx));

        Ok((Self { sender, read_task }, incoming_rx))
    }

    /// Sends a text frame.
    pub async fn send_text(&self, payload: impl Into<String>) -> Result<(), Error> {
        self.sender
            .lock()
            .await
            .send(Message::Text(payload.into()))
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))
    }

    /// Sends a binary frame.
    pub async fn send_binary(&self, payload: Vec<u8>) -> Result<(), Error> {
        self.sender
            .lock()
            .await
            .send(Message::Binary(payload))
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))
    }

    /// Closes the WebSocket connection.
    pub async fn close(self) -> Result<(), Error> {
        self.sender
            .lock()
            .await
            .close()
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))?;
        self.read_task.abort();
        Ok(())
    }
}

async fn read_loop(
    mut read: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    incoming_tx: mpsc::UnboundedSender<TransportMessage>,
) {
    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                if incoming_tx.send(TransportMessage::Text(text)).is_err() {
                    break;
                }
            }
            Ok(Message::Binary(data)) => {
                if incoming_tx.send(TransportMessage::Binary(data)).is_err() {
                    break;
                }
            }
            Ok(Message::Ping(_payload)) => {}
            Ok(Message::Pong(_)) => {}
            Ok(Message::Frame(_)) => {}
            Ok(Message::Close(frame)) => {
                let reason = frame.map(|frame| frame.reason.to_string());
                let _ = incoming_tx.send(TransportMessage::Closed(reason));
                break;
            }
            Err(error) => {
                error!(?error, "websocket read error");
                let _ = incoming_tx.send(TransportMessage::Closed(Some(error.to_string())));
                break;
            }
        }
    }

    let _ = incoming_tx.send(TransportMessage::Closed(None));
}

#[cfg(test)]
mod tests {
    use super::super::append_access_token;

    #[test]
    fn append_access_token_adds_query_parameter() {
        let url = append_access_token("https://example.com/signalr?id=abc", Some("token"))
            .unwrap();
        assert!(url.contains("access_token=token"));
        assert!(url.contains("id=abc"));
    }

    #[test]
    fn append_access_token_skips_empty_token() {
        let url = append_access_token("https://example.com/signalr", Some("")).unwrap();
        assert_eq!(url, "https://example.com/signalr");
    }
}
