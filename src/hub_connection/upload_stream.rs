//! Client-to-server streaming hub method arguments.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{RwLock, mpsc};
use tracing::warn;

use crate::hub::message::{CompletionMessage, HubMessage, StreamItemMessage};
use crate::hub_connection::runtime::HubRuntime;

/// A hub method argument that is either a JSON value or a client upload stream.
pub enum HubArgument {
    /// A regular JSON-serializable argument.
    Value(Value),
    /// A stream of items uploaded to the server separately via `StreamItem` messages.
    Stream(ClientUploadStream),
}

impl std::fmt::Debug for HubArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(value) => write!(f, "Value({value:?})"),
            Self::Stream(_) => write!(f, "Stream(_)"),
        }
    }
}

impl From<Value> for HubArgument {
    fn from(value: Value) -> Self {
        Self::Value(value)
    }
}

impl From<ClientUploadStream> for HubArgument {
    fn from(stream: ClientUploadStream) -> Self {
        Self::Stream(stream)
    }
}

/// Stream of values sent from the client to the server for a hub method argument.
pub struct ClientUploadStream {
    inner: Pin<Box<dyn Stream<Item = Result<Value, String>> + Send>>,
}

impl ClientUploadStream {
    /// Wraps a fallible stream of JSON values.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Value, String>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// Wraps an infallible stream of JSON values.
    pub fn from_values<S>(stream: S) -> Self
    where
        S: Stream<Item = Value> + Send + 'static,
    {
        Self::from_stream(stream.map(Ok))
    }

    /// Creates a stream backed by an unbounded channel receiver.
    pub fn from_receiver(receiver: mpsc::UnboundedReceiver<Value>) -> Self {
        Self::from_values(ReceiverStream(receiver))
    }
}

/// Creates a stream from JSON values.
///
/// The values are collected up front, so the iterator does not have to be `Send` or
/// `'static`. For lazy or unbounded sources use [`ClientUploadStream::from_values`] or
/// [`ClientUploadStream::from_receiver`].
impl FromIterator<Value> for ClientUploadStream {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> Self {
        let items: Vec<Value> = items.into_iter().collect();
        Self::from_values(futures_util::stream::iter(items))
    }
}

struct ReceiverStream(mpsc::UnboundedReceiver<Value>);

impl Stream for ReceiverStream {
    type Item = Value;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

pub(crate) fn replace_streaming_params(
    arguments: Vec<HubArgument>,
    mut next_id: impl FnMut() -> String,
) -> (
    Vec<Value>,
    Option<Vec<String>>,
    HashMap<String, ClientUploadStream>,
) {
    let mut values = Vec::new();
    let mut stream_ids = Vec::new();
    let mut streams = HashMap::new();

    for argument in arguments {
        match argument {
            HubArgument::Value(value) => values.push(value),
            HubArgument::Stream(stream) => {
                let id = next_id();
                stream_ids.push(id.clone());
                streams.insert(id, stream);
            }
        }
    }

    let stream_ids = if stream_ids.is_empty() {
        None
    } else {
        Some(stream_ids)
    };

    (values, stream_ids, streams)
}

enum UploadCommand {
    Item {
        stream_id: String,
        item: Value,
    },
    Complete {
        stream_id: String,
        error: Option<String>,
    },
}

/// Sends the items of client upload streams in the background; call it after the
/// invocation that announces the streams has been sent.
pub(crate) fn launch_upload_streams(
    runtime: Arc<RwLock<Option<HubRuntime>>>,
    streams: HashMap<String, ClientUploadStream>,
) {
    if streams.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        for (stream_id, stream) in streams {
            let command_tx = command_tx.clone();
            tokio::spawn(async move {
                let mut stream = stream.inner;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(value) => {
                            if command_tx
                                .send(UploadCommand::Item {
                                    stream_id: stream_id.clone(),
                                    item: value,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = command_tx.send(UploadCommand::Complete {
                                stream_id,
                                error: Some(error),
                            });
                            return;
                        }
                    }
                }

                let _ = command_tx.send(UploadCommand::Complete {
                    stream_id,
                    error: None,
                });
            });
        }

        drop(command_tx);

        while let Some(command) = command_rx.recv().await {
            let runtime_guard = runtime.read().await;
            let Some(active_runtime) = runtime_guard.as_ref() else {
                break;
            };

            let message = match command {
                UploadCommand::Item { stream_id, item } => {
                    HubMessage::StreamItem(StreamItemMessage {
                        headers: Default::default(),
                        invocation_id: Some(stream_id),
                        item,
                    })
                }
                UploadCommand::Complete { stream_id, error } => {
                    HubMessage::Completion(CompletionMessage {
                        headers: Default::default(),
                        invocation_id: stream_id,
                        error,
                        result: None,
                    })
                }
            };

            if let Err(error) = active_runtime.send(&message).await {
                warn!(?error, "failed to send client upload stream hub message");
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::JsonProtocol;
    use crate::transport::{TransferFormat, WebSocketTransport};
    use futures_util::stream;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn replace_streaming_params_removes_streams_and_assigns_ids() {
        let mut counter = 0_i64;
        let (values, stream_ids, streams) = replace_streaming_params(
            vec![
                HubArgument::Value(Value::String("arg".into())),
                HubArgument::Stream(ClientUploadStream::from_iter([Value::Number(1.into())])),
                HubArgument::Stream(ClientUploadStream::from_iter([Value::Number(2.into())])),
            ],
            || {
                counter += 1;
                counter.to_string()
            },
        );

        assert_eq!(values, vec![Value::String("arg".into())]);
        assert_eq!(stream_ids, Some(vec!["1".into(), "2".into()]));
        assert_eq!(streams.len(), 2);
        assert!(streams.contains_key("1"));
        assert!(streams.contains_key("2"));
    }

    #[tokio::test]
    async fn launch_upload_streams_sends_items_after_initial_message() {
        let (transport, outgoing) = WebSocketTransport::recording();
        let (transport_events, _events_rx) = mpsc::unbounded_channel();
        let runtime = HubRuntime::start(crate::hub_connection::runtime::HubRuntimeOptions {
            transport,
            incoming: mpsc::unbounded_channel().1,
            hub_protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            initial_messages: Vec::new(),
            server_timeout: crate::hub_connection::runtime::DEFAULT_SERVER_TIMEOUT,
            keep_alive_interval: crate::hub_connection::runtime::DEFAULT_KEEP_ALIVE_INTERVAL,
            bus: crate::bus::MessageBus::default(),
            message_buffer: None,
            transport_events,
        })
        .unwrap();

        let runtime = Arc::new(RwLock::new(Some(runtime)));
        let initial_sent = Arc::new(AtomicBool::new(false));
        let initial_sent_clone = initial_sent.clone();
        let runtime_for_initial = runtime.clone();

        let initial_send = async move {
            initial_sent_clone.store(true, Ordering::Relaxed);
            let runtime = runtime_for_initial.read().await;
            runtime
                .as_ref()
                .unwrap()
                .send(&HubMessage::Invocation(
                    crate::hub::message::InvocationMessage {
                        headers: Default::default(),
                        invocation_id: Some("99".into()),
                        target: "Upload".into(),
                        arguments: Default::default(),
                        stream_ids: Some(vec!["1".into()]),
                    },
                ))
                .await
        };

        let mut streams = HashMap::new();
        streams.insert(
            "1".into(),
            ClientUploadStream::from_iter([
                Value::String("first".into()),
                Value::String("second".into()),
            ]),
        );

        initial_send.await.unwrap();
        launch_upload_streams(runtime, streams);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(initial_sent.load(Ordering::Relaxed));
        let messages = outgoing.lock().await;
        assert_eq!(messages.len(), 4);
        assert!(messages[0].contains("\"type\":1"));
        assert!(messages[1].contains("first"));
        assert!(messages[2].contains("second"));
        assert!(messages[3].contains("\"type\":3"));
    }

    #[tokio::test]
    async fn launch_upload_streams_completes_stream_on_error_item() {
        let (transport, outgoing) = WebSocketTransport::recording();
        let (transport_events, _events_rx) = mpsc::unbounded_channel();
        let runtime = HubRuntime::start(crate::hub_connection::runtime::HubRuntimeOptions {
            transport,
            incoming: mpsc::unbounded_channel().1,
            hub_protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            initial_messages: Vec::new(),
            server_timeout: crate::hub_connection::runtime::DEFAULT_SERVER_TIMEOUT,
            keep_alive_interval: crate::hub_connection::runtime::DEFAULT_KEEP_ALIVE_INTERVAL,
            bus: crate::bus::MessageBus::default(),
            message_buffer: None,
            transport_events,
        })
        .unwrap();

        let runtime = Arc::new(RwLock::new(Some(runtime)));
        let runtime_for_initial = runtime.clone();
        let initial_send = async move {
            let runtime = runtime_for_initial.read().await;
            runtime
                .as_ref()
                .unwrap()
                .send(&HubMessage::Invocation(
                    crate::hub::message::InvocationMessage {
                        headers: Default::default(),
                        invocation_id: None,
                        target: "Upload".into(),
                        arguments: Default::default(),
                        stream_ids: Some(vec!["1".into()]),
                    },
                ))
                .await
        };

        let mut streams = HashMap::new();
        streams.insert(
            "1".into(),
            ClientUploadStream::from_stream(stream::iter([
                Ok(Value::String("only".into())),
                Err("boom".into()),
            ])),
        );

        initial_send.await.unwrap();
        launch_upload_streams(runtime, streams);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = outgoing.lock().await;
        assert_eq!(messages.len(), 3);
        assert!(messages[1].contains("only"));
        assert!(messages[2].contains("boom"));
    }
}
