//! Internal message bus for hub invocations and server events.

use crate::error::Error;
use futures_channel::{mpsc, oneshot};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A pending invocation awaiting a response from the server.
pub enum PendingInvocation {
    /// Awaiting a single `Completion` message.
    Single(oneshot::Sender<Result<Value, String>>),
    /// Receiving a stream of `StreamItem` messages until `Completion`.
    Stream(mpsc::UnboundedSender<Result<Value, String>>),
}

/// Events received from the SignalR server.
#[derive(Debug)]
pub enum ServerEvent {
    Invocation {
        target: String,
        arguments: Vec<Value>,
    },
    Completion {
        id: String,
        result: Option<Value>,
        error: Option<String>,
    },
}

/// Coordinates pending invocations and server event handlers.
#[derive(Clone)]
pub struct MessageBus {
    pub(crate) handlers: Arc<RwLock<HashMap<String, Box<dyn Fn(Vec<Value>) + Send + Sync>>>>,
    pub(crate) pending: Arc<RwLock<HashMap<String, PendingInvocation>>>,
    #[allow(dead_code)]
    pub(crate) event_tx: mpsc::UnboundedSender<ServerEvent>,
}

impl MessageBus {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ServerEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded();
        let bus = Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        };
        (bus, event_rx)
    }

    pub async fn register_handler<F>(&self, target: String, handler: F)
    where
        F: Fn(Vec<Value>) + Send + Sync + 'static,
    {
        self.handlers
            .write()
            .await
            .insert(target, Box::new(handler));
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

    pub async fn dispatch_invocation(&self, target: String, arguments: Vec<Value>) {
        if let Some(handler) = self.handlers.read().await.get(&target) {
            handler(arguments);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn register_and_dispatch_handler() {
        let (bus, _rx) = MessageBus::new();
        let target = "TestEvent".to_string();
        let args = vec![Value::String("hello".to_string())];

        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let args_clone = args.clone();

        bus.register_handler(target.clone(), move |received_args| {
            assert_eq!(received_args, args_clone);
            called_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .await;

        bus.dispatch_invocation(target, args).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(called.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn register_and_complete_invocation() {
        let (bus, _rx) = MessageBus::new();
        let id = "test_id".to_string();
        let result = Ok(Value::String("success".to_string()));

        let rx = bus.register_pending(id.clone()).await.unwrap();
        bus.complete_invocation(id, result.clone()).await;

        assert_eq!(rx.await.unwrap(), result);
    }

    #[tokio::test]
    async fn complete_nonexistent_invocation() {
        let (bus, _rx) = MessageBus::new();
        bus.complete_invocation("nonexistent".to_string(), Ok(Value::Null))
            .await;
    }

    #[tokio::test]
    async fn register_and_stream_items() {
        let (bus, _rx) = MessageBus::new();
        let id = "stream_id".to_string();
        let mut rx = bus.register_stream(id.clone()).await.unwrap();

        bus.send_stream_item(id.clone(), Ok(Value::Number(42.into())))
            .await;
        bus.send_stream_item(id, Ok(Value::String("done".to_string())))
            .await;

        assert_eq!(
            rx.try_next().unwrap(),
            Some(Ok(Value::Number(42.into())))
        );
        assert_eq!(
            rx.try_next().unwrap(),
            Some(Ok(Value::String("done".to_string())))
        );
    }

    #[tokio::test]
    async fn stream_completion_with_error() {
        let (bus, _rx) = MessageBus::new();
        let id = "stream_id".to_string();
        let mut rx = bus.register_stream(id.clone()).await.unwrap();

        bus.complete_invocation(id, Err("Stream failed".to_string()))
            .await;

        let result = rx.next().await.unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Stream failed");
    }
}
