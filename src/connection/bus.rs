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
/// Currently supports only single-result invocations.
/// Streaming invocations will be added in Phase 4.
pub enum PendingInvocation {
    /// Awaiting a single `Completion` message.
    Single(oneshot::Sender<Result<Value, String>>),
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
        if let Some(PendingInvocation::Single(tx)) = self.pending.write().await.remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// Dispatches a server event to the registered handler.
    pub async fn dispatch_invocation(&self, target: String, arguments: Vec<Value>) {
        if let Some(handler) = self.handlers.read().await.get(&target) {
            handler(arguments);
        }
    }
}
