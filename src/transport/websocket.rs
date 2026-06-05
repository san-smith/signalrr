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

#[derive(Clone)]
struct WebSocketTransportInner {
    sender: Option<
        Arc<
            Mutex<
                futures_util::stream::SplitSink<
                    WebSocketStream<MaybeTlsStream<TcpStream>>,
                    Message,
                >,
            >,
        >,
    >,
    read_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    closed: Arc<Mutex<bool>>,
    #[cfg(test)]
    test_outgoing: Option<Arc<Mutex<Vec<String>>>>,
}

/// WebSocket transport handle.
#[derive(Clone)]
pub struct WebSocketTransport {
    inner: Option<WebSocketTransportInner>,
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

        Ok((
            Self {
                inner: Some(WebSocketTransportInner {
                    sender: Some(sender),
                    read_task: Arc::new(Mutex::new(Some(read_task))),
                    closed: Arc::new(Mutex::new(false)),
                    #[cfg(test)]
                    test_outgoing: None,
                }),
            },
            incoming_rx,
        ))
    }

    /// Sends a text frame.
    pub async fn send_text(&self, payload: impl Into<String>) -> Result<(), Error> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        #[cfg(test)]
        if let Some(outgoing) = &inner.test_outgoing {
            outgoing.lock().await.push(payload.into());
            return Ok(());
        }

        let Some(sender) = &inner.sender else {
            return Ok(());
        };

        sender
            .lock()
            .await
            .send(Message::Text(payload.into()))
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))
    }

    /// Sends a binary frame.
    pub async fn send_binary(&self, payload: Vec<u8>) -> Result<(), Error> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        let Some(sender) = &inner.sender else {
            return Ok(());
        };

        sender
            .lock()
            .await
            .send(Message::Binary(payload))
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))
    }

    /// Closes the WebSocket connection.
    pub async fn close(&self) -> Result<(), Error> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        let mut closed = inner.closed.lock().await;
        if *closed {
            return Ok(());
        }
        *closed = true;

        if let Some(sender) = &inner.sender {
            sender
                .lock()
                .await
                .close()
                .await
                .map_err(|error| Error::WebSocket(Box::new(error)))?;
        }

        if let Some(task) = inner.read_task.lock().await.take() {
            task.abort();
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn disconnected() -> Self {
        Self { inner: None }
    }

    #[cfg(test)]
    pub(crate) fn recording() -> (Self, Arc<Mutex<Vec<String>>>) {
        let outgoing = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner: Some(WebSocketTransportInner {
                    sender: None,
                    read_task: Arc::new(Mutex::new(None)),
                    closed: Arc::new(Mutex::new(false)),
                    test_outgoing: Some(outgoing.clone()),
                }),
            },
            outgoing,
        )
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
        let url = append_access_token("https://example.com/signalr?id=abc", Some("token")).unwrap();
        assert!(url.contains("access_token=token"));
        assert!(url.contains("id=abc"));
    }

    #[test]
    fn append_access_token_skips_empty_token() {
        let url = append_access_token("https://example.com/signalr", Some("")).unwrap();
        assert_eq!(url, "https://example.com/signalr");
    }
}
