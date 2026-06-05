//! Internal message bus for hub invocations and server events.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_channel::{mpsc, oneshot};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::RwLock;

use crate::error::Error;

/// Error message used when pending invocations are canceled due to connection loss.
pub const INVOCATION_CANCELED_MESSAGE: &str =
    "Invocation canceled due to the underlying connection being closed.";

static NEXT_HANDLER_ID: AtomicU64 = AtomicU64::new(1);

/// Identifier returned when registering a client hub method handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientHandlerId(u64);

/// Output of a client hub method handler invocation.
pub struct ClientMethodOutput {
    inner: Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send>>,
}

impl ClientMethodOutput {
    /// Returns an immediately-ready handler result.
    pub fn ready(result: Result<Option<Value>, String>) -> Self {
        Self {
            inner: Box::pin(async move { result }),
        }
    }

    /// Wraps an asynchronous handler result.
    pub fn r#async<F>(future: F) -> Self
    where
        F: Future<Output = Result<Option<Value>, String>> + Send + 'static,
    {
        Self {
            inner: Box::pin(future),
        }
    }

    pub(crate) async fn await_output(self) -> Result<Option<Value>, String> {
        self.inner.await
    }
}

/// Handler invoked when the server calls a client hub method.
pub type ClientMethodHandler = Arc<dyn Fn(Vec<Value>) -> ClientMethodOutput + Send + Sync>;

struct RegisteredClientHandler {
    id: u64,
    handler: ClientMethodHandler,
}

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
    handlers: Arc<RwLock<HashMap<String, Vec<RegisteredClientHandler>>>>,
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

    pub async fn register_handler(
        &self,
        target: String,
        handler: ClientMethodHandler,
    ) -> ClientHandlerId {
        let id = NEXT_HANDLER_ID.fetch_add(1, Ordering::Relaxed);
        self.handlers
            .write()
            .await
            .entry(target.to_ascii_lowercase())
            .or_default()
            .push(RegisteredClientHandler { id, handler });
        ClientHandlerId(id)
    }

    pub async fn remove_handler(&self, target: &str, handler_id: Option<ClientHandlerId>) {
        let mut handlers = self.handlers.write().await;
        let Some(method_handlers) = handlers.get_mut(&target.to_ascii_lowercase()) else {
            return;
        };

        match handler_id {
            Some(ClientHandlerId(id)) => {
                method_handlers.retain(|handler| handler.id != id);
                if method_handlers.is_empty() {
                    handlers.remove(&target.to_ascii_lowercase());
                }
            }
            None => {
                handlers.remove(&target.to_ascii_lowercase());
            }
        }
    }

    pub async fn handlers_for(&self, target: &str) -> Vec<ClientMethodHandler> {
        self.handlers
            .read()
            .await
            .get(&target.to_ascii_lowercase())
            .map(|handlers| handlers.iter().map(|entry| entry.handler.clone()).collect())
            .unwrap_or_default()
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
                ClientMethodOutput::ready(Ok(None))
            }),
        )
        .await;

        let handlers = bus.handlers_for(&target).await;
        assert_eq!(handlers.len(), 1);
        let _ = handlers[0](args).await_output().await;
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn multiple_handlers_are_registered_for_same_method() {
        let bus = MessageBus::default();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));

        bus.register_handler(
            "Event".to_string(),
            Arc::new({
                let first = first.clone();
                move |_| {
                    first.store(true, Ordering::Relaxed);
                    ClientMethodOutput::ready(Ok(None))
                }
            }),
        )
        .await;
        bus.register_handler(
            "Event".to_string(),
            Arc::new({
                let second = second.clone();
                move |_| {
                    second.store(true, Ordering::Relaxed);
                    ClientMethodOutput::ready(Ok(None))
                }
            }),
        )
        .await;

        assert_eq!(bus.handlers_for("Event").await.len(), 2);
    }

    #[tokio::test]
    async fn remove_handler_can_target_single_registration() {
        let bus = MessageBus::default();
        let keep = Arc::new(AtomicBool::new(false));

        let removed_id = bus
            .register_handler(
                "Event".to_string(),
                Arc::new(|_| ClientMethodOutput::ready(Ok(None))),
            )
            .await;
        bus.register_handler(
            "Event".to_string(),
            Arc::new({
                let keep = keep.clone();
                move |_| {
                    keep.store(true, Ordering::Relaxed);
                    ClientMethodOutput::ready(Ok(None))
                }
            }),
        )
        .await;

        bus.remove_handler("Event", Some(removed_id)).await;

        let handlers = bus.handlers_for("Event").await;
        assert_eq!(handlers.len(), 1);
        let _ = handlers[0](Vec::new()).await_output().await;
        assert!(keep.load(Ordering::Relaxed));
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
        let bus = MessageBus::default();
        let invoke = bus.register_pending("invoke-1".into()).await.unwrap();
        let mut stream = bus.register_stream("stream-1".into()).await.unwrap();

        bus.cancel_all_pending("connection lost").await;

        assert_eq!(invoke.await.unwrap(), Err("connection lost".into()));
        assert_eq!(stream.next().await, Some(Err("connection lost".into())));
        assert!(stream.next().await.is_none());
    }
}
