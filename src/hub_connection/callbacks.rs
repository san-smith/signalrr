//! Lifecycle callbacks for [`super::HubConnection`].

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::RwLock;
use tracing::warn;

/// Called when the connection has fully closed.
pub type CloseCallback = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// Called when the connection begins reconnecting after an unexpected transport close.
pub type ReconnectingCallback = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// Called after a reconnect attempt succeeds.
pub type ReconnectedCallback = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// Registered lifecycle callbacks and connection-started tracking.
#[derive(Default)]
pub struct LifecycleCallbacks {
    closed: RwLock<Vec<CloseCallback>>,
    reconnecting: RwLock<Vec<ReconnectingCallback>>,
    reconnected: RwLock<Vec<ReconnectedCallback>>,
    connection_started: AtomicBool,
}

impl LifecycleCallbacks {
    pub fn mark_started(&self) {
        self.connection_started.store(true, Ordering::Relaxed);
    }

    pub async fn register_closed(&self, callback: CloseCallback) {
        self.closed.write().await.push(callback);
    }

    pub async fn register_reconnecting(&self, callback: ReconnectingCallback) {
        self.reconnecting.write().await.push(callback);
    }

    pub async fn register_reconnected(&self, callback: ReconnectedCallback) {
        self.reconnected.write().await.push(callback);
    }

    pub async fn invoke_closed(&self, error: Option<&str>) {
        if !self.connection_started.swap(false, Ordering::Relaxed) {
            return;
        }

        for callback in self.closed.read().await.iter() {
            invoke_callback("on_close", || callback(error));
        }
    }

    pub async fn invoke_reconnecting(&self, error: Option<&str>) {
        for callback in self.reconnecting.read().await.iter() {
            invoke_callback("on_reconnecting", || callback(error));
        }
    }

    pub async fn invoke_reconnected(&self, connection_id: Option<&str>) {
        for callback in self.reconnected.read().await.iter() {
            invoke_callback("on_reconnected", || callback(connection_id));
        }
    }
}

fn invoke_callback(name: &str, callback: impl FnOnce()) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(callback)) {
        warn!(callback = name, ?panic, "lifecycle callback panicked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn invoke_closed_runs_only_after_connection_started() {
        let callbacks = LifecycleCallbacks::default();
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        callbacks
            .register_closed(Arc::new(move |_| {
                count_clone.fetch_add(1, Ordering::Relaxed);
            }))
            .await;

        callbacks.invoke_closed(None).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        callbacks.mark_started();
        callbacks.invoke_closed(Some("boom")).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);
        callbacks.invoke_closed(None).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn reconnecting_and_reconnected_callbacks_receive_values() {
        let callbacks = LifecycleCallbacks::default();
        let reconnecting_reason = Arc::new(Mutex::new(None));
        let reconnected_id = Arc::new(Mutex::new(None));

        let reconnecting_reason_clone = reconnecting_reason.clone();
        callbacks
            .register_reconnecting(Arc::new(move |error| {
                *reconnecting_reason_clone.lock().unwrap() = error.map(str::to_string);
            }))
            .await;

        let reconnected_id_clone = reconnected_id.clone();
        callbacks
            .register_reconnected(Arc::new(move |connection_id| {
                *reconnected_id_clone.lock().unwrap() = connection_id.map(str::to_string);
            }))
            .await;

        callbacks
            .invoke_reconnecting(Some("transport closed"))
            .await;
        callbacks.invoke_reconnected(Some("abc")).await;

        assert_eq!(
            reconnecting_reason.lock().unwrap().as_deref(),
            Some("transport closed")
        );
        assert_eq!(reconnected_id.lock().unwrap().as_deref(), Some("abc"));
    }
}
