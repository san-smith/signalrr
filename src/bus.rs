//! Internal message bus for hub invocations and server events.

use crate::error::Error;
use futures_channel::{mpsc, oneshot};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Error message used when pending invocations are canceled due to connection loss.
pub const INVOCATION_CANCELED_MESSAGE: &str =
    "Invocation canceled due to the underlying connection being closed.";

/// Handler invoked when the server calls a client hub method.
pub type ClientMethodHandler = Arc<dyn Fn(Vec<Value>) -> Option<Value> + Send + Sync>;

/// A pending invocation awaiting a response from the server.
pub enum PendingInvocation {
    /// Awaiting a single `Completion` message.
    Single(oneshot::Sender<Result<Value, String>>),
    /// Receiving a stream of `StreamItem` messages until `Completion`.
    Stream(mpsc::UnboundedSender<Result<Value, String>>),
}

/// Coordinates pending invocations and server event handlers.
#[derive(Clone)]
pub struct MessageBus {
    handlers: Arc<RwLock<HashMap<String, ClientMethodHandler>>>,
    pending: Arc<RwLock<HashMap<String, PendingInvocation>>>,
}

impl MessageBus {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<()>) {
        let (_event_tx, event_rx) = mpsc::unbounded();
        let bus = Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
        };
        (bus, event_rx)
    }

    pub async fn register_handler(&self, target: String, handler: ClientMethodHandler) {
        self.handlers
            .write()
            .await
            .insert(target.to_ascii_lowercase(), handler);
    }

    pub async fn remove_handler(&self, target: &str) {
        self.handlers
            .write()
            .await
            .remove(&target.to_ascii_lowercase());
    }

    pub async fn handler_for(&self, target: &str) -> Option<ClientMethodHandler> {
        self.handlers
            .read()
            .await
            .get(&target.to_ascii_lowercase())
            .cloned()
    }

    pub async fn register_pending(
        &self,
        id: String,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, Error> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .write()
            .await
            .insert(id, PendingInvocation::Single(tx));
        Ok(rx)
    }

    pub async fn complete_invocation(&self, id: String, result: Result<Value, String>) {
        if let Some(pending) = self.pending.write().await.remove(&id) {
            match pending {
                PendingInvocation::Single(tx) => {
                    let _ = tx.send(result);
                }
                PendingInvocation::Stream(tx) => {
                    if result.is_err() {
                        let _ = tx.unbounded_send(result);
                    }
                }
            }
        }
    }

    pub async fn register_stream(
        &self,
        id: String,
    ) -> Result<mpsc::UnboundedReceiver<Result<Value, String>>, Error> {
        let (tx, rx) = mpsc::unbounded();
        self.pending
            .write()
            .await
            .insert(id, PendingInvocation::Stream(tx));
        Ok(rx)
    }

    pub async fn send_stream_item(&self, id: String, item: Result<Value, String>) {
        if let Some(PendingInvocation::Stream(tx)) = self.pending.read().await.get(&id) {
            let _ = tx.unbounded_send(item);
        }
    }

    pub async fn remove_pending(&self, id: &str) -> bool {
        self.pending.write().await.remove(id).is_some()
    }

    /// Fails all pending invocations and streams with the given error message.
    pub async fn cancel_all_pending(&self, error: impl Into<String>) {
        let error = error.into();
        let pending = self.pending.write().await.drain().collect::<Vec<_>>();

        for (_, invocation) in pending {
            match invocation {
                PendingInvocation::Single(tx) => {
                    let _ = tx.send(Err(error.clone()));
                }
                PendingInvocation::Stream(tx) => {
                    let _ = tx.unbounded_send(Err(error.clone()));
                }
            }
        }
    }
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn register_and_dispatch_handler() {
        let bus = MessageBus::default();
        let target = "TestEvent".to_string();
        let args = vec![Value::String("hello".to_string())];

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let args_clone = args.clone();

        bus.register_handler(
            target.clone(),
            Arc::new(move |received_args| {
                assert_eq!(received_args, args_clone);
                called_clone.store(true, Ordering::Relaxed);
                None
            }),
        )
        .await;

        let handler = bus.handler_for(&target).await.unwrap();
        handler(args);
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn register_and_complete_invocation() {
        let bus = MessageBus::default();
        let id = "test_id".to_string();
        let result = Ok(Value::String("success".to_string()));

        let rx = bus.register_pending(id.clone()).await.unwrap();
        bus.complete_invocation(id, result.clone()).await;

        assert_eq!(rx.await.unwrap(), result);
    }

    #[tokio::test]
    async fn complete_nonexistent_invocation() {
        let bus = MessageBus::default();
        bus.complete_invocation("nonexistent".to_string(), Ok(Value::Null))
            .await;
    }

    #[tokio::test]
    async fn register_and_stream_items() {
        let bus = MessageBus::default();
        let id = "stream_id".to_string();
        let mut rx = bus.register_stream(id.clone()).await.unwrap();

        bus.send_stream_item(id.clone(), Ok(Value::Number(42.into())))
            .await;
        bus.send_stream_item(id, Ok(Value::String("done".to_string())))
            .await;

        assert_eq!(rx.try_next().unwrap(), Some(Ok(Value::Number(42.into()))));
        assert_eq!(
            rx.try_next().unwrap(),
            Some(Ok(Value::String("done".to_string())))
        );
    }

    #[tokio::test]
    async fn stream_completion_with_error() {
        let bus = MessageBus::default();
        let id = "stream_id".to_string();
        let mut rx = bus.register_stream(id.clone()).await.unwrap();

        bus.complete_invocation(id, Err("Stream failed".to_string()))
            .await;

        let result = rx.next().await.unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Stream failed");
    }

    #[tokio::test]
    async fn cancel_all_pending_fails_invoke_and_stream() {
        use futures_util::StreamExt;

        let bus = MessageBus::default();
        let invoke = bus.register_pending("invoke-1".into()).await.unwrap();
        let mut stream = bus.register_stream("stream-1".into()).await.unwrap();

        bus.cancel_all_pending("connection lost").await;

        assert_eq!(invoke.await.unwrap(), Err("connection lost".into()));
        assert_eq!(
            stream.next().await,
            Some(Err("connection lost".into()))
        );
        assert!(stream.next().await.is_none());
    }
}
