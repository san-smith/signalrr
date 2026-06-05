//! Server-to-client streaming hub invocations.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::mpsc;
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::bus::MessageBus;
use crate::error::Error;
use crate::hub::message::{CancelInvocationMessage, HubMessage};
use crate::hub_connection::runtime::HubRuntime;

/// Stream of items returned by a server hub method invoked via [`super::HubConnection::stream`].
pub struct HubMethodStream {
    invocation_id: String,
    items: mpsc::UnboundedReceiver<Result<Value, String>>,
    runtime: Arc<RwLock<Option<HubRuntime>>>,
    bus: MessageBus,
}

impl HubMethodStream {
    pub(crate) fn new(
        invocation_id: String,
        items: mpsc::UnboundedReceiver<Result<Value, String>>,
        runtime: Arc<RwLock<Option<HubRuntime>>>,
        bus: MessageBus,
    ) -> Self {
        Self {
            invocation_id,
            items,
            runtime,
            bus,
        }
    }

    /// Server-assigned invocation identifier for this stream.
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    /// Cancels the stream and notifies the server.
    pub async fn cancel(mut self) -> Result<(), Error> {
        self.items.close();
        self.send_cancel().await
    }

    async fn send_cancel(&self) -> Result<(), Error> {
        self.bus.remove_pending(&self.invocation_id).await;

        let runtime = self.runtime.read().await;
        let runtime = runtime.as_ref().ok_or(Error::NotConnected)?;
        runtime
            .send(&HubMessage::CancelInvocation(CancelInvocationMessage {
                headers: Default::default(),
                invocation_id: self.invocation_id.clone(),
            }))
            .await
    }
}

impl Stream for HubMethodStream {
    type Item = Result<Value, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.items)
            .poll_next(cx)
            .map(|item| item.map(|result| result.map_err(Error::Invocation)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn stream_maps_bus_items_and_errors() {
        let bus = MessageBus::default();
        let id = "stream-1".to_string();
        let items = bus.register_stream(id.clone()).await.unwrap();
        let mut stream =
            HubMethodStream::new(id.clone(), items, Arc::new(RwLock::new(None)), bus.clone());

        bus.send_stream_item(id.clone(), Ok(Value::Number(1.into())))
            .await;
        bus.complete_invocation(id, Err("failed".into())).await;

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Value::Number(1.into())
        );
        assert!(matches!(
            stream.next().await,
            Some(Err(Error::Invocation(message))) if message == "failed"
        ));
        assert!(stream.next().await.is_none());
    }
}
