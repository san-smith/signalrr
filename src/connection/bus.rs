//! Internal message bus for handling SignalR invocations and events.
//!
//! This module provides the core infrastructure for:
//! - Managing pending client invocations (`invoke` → `Completion`),
//! - Dispatching server events (`on` → `Invocation`).
//!
//! It uses `Arc<RwLock<...>>` for thread-safe shared state and `futures-channel`
//! for asynchronous communication.

use crate::error::SignalRError;
use futures_channel::{mpsc, oneshot};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A pending invocation awaiting a response from the server.
///
/// Supports both single-result and streaming invocations.
pub enum PendingInvocation {
    /// Awaiting a single `Completion` message.
    Single(oneshot::Sender<Result<Value, String>>),
    /// Receiving a stream of `StreamItem` messages until `Completion`.
    Stream(mpsc::UnboundedSender<Result<Value, String>>),
}

/// Events received from the SignalR server.
#[derive(Debug)]
pub enum ServerEvent {
    /// A server-to-client method call (e.g., event broadcast).
    Invocation {
        target: String,
        arguments: Vec<Value>,
    },
    /// The result of a client invocation.
    Completion {
        id: String,
        result: Option<Value>,
        error: Option<String>,
    },
}

/// Internal message bus for the connection.
///
/// This struct is not exposed to the user. It is used internally by `Connection`
/// to coordinate between the WebSocket reader task and the public API.
#[derive(Clone)]
pub struct MessageBus {
    /// Registered event handlers (e.g., for `on("method", ...)`).
    pub(crate) handlers: Arc<RwLock<HashMap<String, Box<dyn Fn(Vec<Value>) + Send + Sync>>>>,
    /// Pending invocations awaiting `Completion`.
    pub(crate) pending: Arc<RwLock<HashMap<String, PendingInvocation>>>,
    /// Channel to forward server events (currently unused in this design).
    pub(crate) event_tx: mpsc::UnboundedSender<ServerEvent>,
}

impl MessageBus {
    /// Creates a new message bus.
    ///
    /// Returns the bus and a receiver for server events (reserved for future use).
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ServerEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded();
        let bus = Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        };
        (bus, event_rx)
    }

    /// Registers a handler for a server event.
    pub async fn register_handler<F>(&self, target: String, handler: F)
    where
        F: Fn(Vec<Value>) + Send + Sync + 'static,
    {
        self.handlers
            .write()
            .await
            .insert(target, Box::new(handler));
    }

    /// Registers a pending invocation and returns a receiver for the result.
    pub async fn register_pending(
        &self,
        id: String,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, SignalRError> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .write()
            .await
            .insert(id, PendingInvocation::Single(tx));
        Ok(rx)
    }

    /// Completes a pending invocation with a result or error.
    pub async fn complete_invocation(&self, id: String, result: Result<Value, String>) {
        if let Some(pending) = self.pending.write().await.remove(&id) {
            match pending {
                PendingInvocation::Single(tx) => {
                    let _ = tx.send(result);
                }
                PendingInvocation::Stream(tx) => {
                    // Для стрима: отправляем ошибку или завершаем канал
                    if result.is_err() {
                        let _ = tx.unbounded_send(result);
                    }
                    // Успешное завершение: отправляем `None` через отдельный канал
                    // Но в текущей модели — просто закрываем канал
                }
            }
        }
    }

    /// Dispatches a server event to the registered handler.
    pub async fn dispatch_invocation(&self, target: String, arguments: Vec<Value>) {
        if let Some(handler) = self.handlers.read().await.get(&target) {
            handler(arguments);
        }
    }

    /// Registers a pending streaming invocation and returns a receiver for stream items.
    ///
    /// The receiver will yield items until the server sends a `Completion` message.
    pub async fn register_stream(
        &self,
        id: String,
    ) -> Result<mpsc::UnboundedReceiver<Result<Value, String>>, SignalRError> {
        let (tx, rx) = mpsc::unbounded();
        self.pending
            .write()
            .await
            .insert(id, PendingInvocation::Stream(tx));
        Ok(rx)
    }

    /// Sends a stream item to a pending streaming invocation.
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
    use serde_json::Value;

    #[tokio::test]
    async fn test_register_and_dispatch_handler() {
        let (bus, _rx) = MessageBus::new();
        let target = "TestEvent".to_string();
        let args = vec![Value::String("hello".to_string())];

        // Флаг для проверки вызова
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        let args_clone = args.clone();
        bus.register_handler(target.clone(), move |received_args| {
            assert_eq!(received_args, args_clone);
            called_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .await;

        bus.dispatch_invocation(target, args).await;

        // Даем время обработчику выполниться
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(called.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_register_and_complete_invocation() {
        let (bus, _rx) = MessageBus::new();
        let id = "test_id".to_string();
        let result = Ok(Value::String("success".to_string()));

        let rx = bus.register_pending(id.clone()).await.unwrap();
        bus.complete_invocation(id, result.clone()).await;

        let received = rx.await.unwrap();
        assert_eq!(received, result);
    }

    #[tokio::test]
    async fn test_complete_nonexistent_invocation() {
        let (bus, _rx) = MessageBus::new();
        // Должно завершиться без паники
        bus.complete_invocation("nonexistent".to_string(), Ok(Value::Null))
            .await;
    }

    #[tokio::test]
    async fn test_register_and_stream_items() {
        let (bus, _rx) = MessageBus::new();
        let id = "stream_id".to_string();

        let rx = bus.register_stream(id.clone()).await.unwrap();

        // Отправляем элементы
        bus.send_stream_item(id.clone(), Ok(Value::Number(42.into())))
            .await;
        bus.send_stream_item(id.clone(), Ok(Value::String("done".to_string())))
            .await;

        // Получаем элементы
        let mut items = Vec::new();
        let mut rx = rx;
        if let Ok(item) = rx.try_next() {
            items.push(item.unwrap());
        }
        if let Ok(item) = rx.try_next() {
            items.push(item.unwrap());
        }

        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Ok(Value::Number(42.into())));
        assert_eq!(items[1], Ok(Value::String("done".to_string())));
    }

    #[tokio::test]
    async fn test_stream_completion() {
        let (bus, _rx) = MessageBus::new();
        let id = "stream_id".to_string();

        let mut rx = bus.register_stream(id.clone()).await.unwrap();

        // Завершаем стрим с ошибкой
        bus.complete_invocation(id, Err("Stream failed".to_string()))
            .await;

        // Получаем первый (и единственный) элемент — ошибку завершения
        let result = rx.next().await.unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Stream failed");
    }
}
