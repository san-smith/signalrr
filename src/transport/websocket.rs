//! WebSocket transport for SignalR.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tracing::{debug, error, info, trace};

use crate::error::Error;
use crate::transport::send_queue::TransportSendQueue;
use crate::transport::{TransportPayloadDetail, format_data_detail};

/// Options for opening a WebSocket transport connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebSocketConnectOptions {
    pub log_message_content: bool,
}

/// A message received from the WebSocket transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMessage {
    Text(String),
    Binary(Vec<u8>),
    Closed(Option<String>),
}

type WebSocketSink =
    futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

#[derive(Clone)]
struct WebSocketTransportInner {
    sender: Option<Arc<Mutex<WebSocketSink>>>,
    read_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    closed: Arc<Mutex<bool>>,
    log_message_content: bool,
    send_queue: Option<Arc<TransportSendQueue>>,
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
        Self::connect_with_options(url, access_token, WebSocketConnectOptions::default()).await
    }

    /// Opens a WebSocket connection with explicit transport options.
    pub async fn connect_with_options(
        url: &str,
        access_token: Option<&str>,
        options: WebSocketConnectOptions,
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
        let log_message_content = options.log_message_content;

        let read_task = tokio::spawn(read_loop(read, incoming_tx, log_message_content));

        Ok((
            Self {
                inner: Some(WebSocketTransportInner {
                    sender: Some(sender),
                    read_task: Arc::new(Mutex::new(Some(read_task))),
                    closed: Arc::new(Mutex::new(false)),
                    log_message_content,
                    send_queue: None,
                    #[cfg(test)]
                    test_outgoing: None,
                }),
            },
            incoming_rx,
        ))
    }

    pub(crate) fn clone_without_send_queue(&self) -> Self {
        Self {
            inner: self.inner.as_ref().map(|inner| WebSocketTransportInner {
                sender: inner.sender.clone(),
                read_task: inner.read_task.clone(),
                closed: inner.closed.clone(),
                log_message_content: inner.log_message_content,
                send_queue: None,
                #[cfg(test)]
                test_outgoing: inner.test_outgoing.clone(),
            }),
        }
    }

    pub fn enable_send_queue(&mut self) {
        if self
            .inner
            .as_ref()
            .is_some_and(|inner| inner.send_queue.is_some())
        {
            return;
        }

        let direct_transport = self.clone_without_send_queue();
        let Some(inner) = self.inner.as_mut() else {
            return;
        };

        inner.send_queue = Some(Arc::new(TransportSendQueue::new(direct_transport)));
    }

    /// Sends a text frame.
    pub async fn send_text(&self, payload: impl Into<String>) -> Result<(), Error> {
        let payload = payload.into();
        if let Some(queue) = self.send_queue()? {
            return queue.send_text(payload).await;
        }
        self.send_text_direct(payload).await
    }

    pub(crate) async fn send_text_direct(&self, payload: String) -> Result<(), Error> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        trace!(
            detail = %format_data_detail(
                &TransportPayloadDetail::Text(&payload),
                inner.log_message_content,
            ),
            "websocket sending text data"
        );

        #[cfg(test)]
        if let Some(outgoing) = &inner.test_outgoing {
            outgoing.lock().await.push(payload);
            return Ok(());
        }

        let Some(sender) = &inner.sender else {
            return Ok(());
        };

        sender
            .lock()
            .await
            .send(Message::Text(payload))
            .await
            .map_err(|error| Error::WebSocket(Box::new(error)))
    }

    /// Sends a binary frame.
    pub async fn send_binary(&self, payload: Vec<u8>) -> Result<(), Error> {
        if let Some(queue) = self.send_queue()? {
            return queue.send_binary(payload).await;
        }
        self.send_binary_direct(payload).await
    }

    pub(crate) async fn send_binary_direct(&self, payload: Vec<u8>) -> Result<(), Error> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        trace!(
            detail = %format_data_detail(
                &TransportPayloadDetail::Binary(&payload),
                inner.log_message_content,
            ),
            "websocket sending binary data"
        );

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

        if let Some(queue) = &inner.send_queue {
            queue.stop().await;
        }

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

    fn send_queue(&self) -> Result<Option<&Arc<TransportSendQueue>>, Error> {
        Ok(self
            .inner
            .as_ref()
            .and_then(|inner| inner.send_queue.as_ref()))
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
                    log_message_content: false,
                    send_queue: None,
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
    log_message_content: bool,
) {
    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                trace!(
                    detail = %format_data_detail(
                        &TransportPayloadDetail::Text(&text),
                        log_message_content,
                    ),
                    "websocket received text data"
                );
                if incoming_tx.send(TransportMessage::Text(text)).is_err() {
                    break;
                }
            }
            Ok(Message::Binary(data)) => {
                trace!(
                    detail = %format_data_detail(
                        &TransportPayloadDetail::Binary(&data),
                        log_message_content,
                    ),
                    "websocket received binary data"
                );
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
