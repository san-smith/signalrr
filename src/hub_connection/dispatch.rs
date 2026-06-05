//! Dispatches parsed hub messages to the message bus and client handlers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;

use crate::bus::{MessageBus, INVOCATION_CANCELED_MESSAGE};
use crate::error::Error;
use crate::hub::message::{
    CloseMessage, CompletionMessage, HubMessage, InvocationMessage,
    StreamItemMessage,
};
use crate::hub::protocol::HubProtocol;
use crate::hub_connection::message_buffer::MessageBuffer;
use crate::hub_connection::runtime::{TransportCloseInfo, TransportEvent};
use crate::transport::{TransferFormat, WebSocketTransport};

/// Shared context for inbound hub message processing.
pub struct DispatchContext {
    pub bus: MessageBus,
    pub transport: WebSocketTransport,
    pub protocol: Arc<dyn HubProtocol>,
    pub transfer_format: TransferFormat,
    pub message_buffer: Option<Arc<MessageBuffer>>,
    pub transport_events: mpsc::UnboundedSender<TransportEvent>,
    pub transport_close_signaled: Arc<AtomicBool>,
}

impl DispatchContext {
    pub async fn dispatch(&self, message: HubMessage) -> Result<(), Error> {
        if let Some(buffer) = &self.message_buffer {
            if !buffer
                .should_process_message(&message, &self.transport, self.transfer_format)
                .await
            {
                return Ok(());
            }
        }

        match message {
            HubMessage::Invocation(message) => self.handle_invocation(message).await,
            HubMessage::StreamItem(message) => {
                self.handle_stream_item(message).await;
                Ok(())
            }
            HubMessage::Completion(message) => {
                self.handle_completion(message).await;
                Ok(())
            }
            HubMessage::Ping(_) => Ok(()),
            HubMessage::Close(message) => self.handle_close(message).await,
            HubMessage::Ack(message) => {
                if let Some(buffer) = &self.message_buffer {
                    buffer.ack(message).await;
                }
                Ok(())
            }
            HubMessage::Sequence(message) => {
                if let Some(buffer) = &self.message_buffer {
                    buffer.reset_sequence(message).await?;
                }
                Ok(())
            }
            HubMessage::StreamInvocation(_) | HubMessage::CancelInvocation(_) => Ok(()),
        }
    }

    async fn handle_invocation(&self, message: InvocationMessage) -> Result<(), Error> {
        let target = message.target.to_ascii_lowercase();
        let handler = self.bus.handler_for(&target).await;

        let Some(handler) = handler else {
            if let Some(invocation_id) = message.invocation_id {
                warn!(method = %message.target, "no client handler registered for server invocation");
                self.send_completion(CompletionMessage {
                    headers: Default::default(),
                    invocation_id,
                    error: Some("Client didn't provide a result.".into()),
                    result: None,
                })
                .await?;
            }
            return Ok(());
        };

        let response = handler(message.arguments);
        if let Some(invocation_id) = message.invocation_id {
            let completion = match response {
                Some(result) => CompletionMessage {
                    headers: Default::default(),
                    invocation_id,
                    error: None,
                    result: Some(result),
                },
                None => CompletionMessage {
                    headers: Default::default(),
                    invocation_id,
                    error: Some("Client didn't provide a result.".into()),
                    result: None,
                },
            };
            self.send_completion(completion).await?;
        } else if response.is_some() {
            warn!(
                method = %message.target,
                "client handler returned a result for a server invocation that did not expect one"
            );
        }

        Ok(())
    }

    async fn handle_stream_item(&self, message: StreamItemMessage) {
        let Some(invocation_id) = message.invocation_id else {
            return;
        };

        self.bus
            .send_stream_item(invocation_id, Ok(message.item))
            .await;
    }

    async fn handle_completion(&self, message: CompletionMessage) {
        let result = if let Some(error) = message.error {
            Err(error)
        } else {
            Ok(message.result.unwrap_or(Value::Null))
        };

        self.bus
            .complete_invocation(message.invocation_id, result)
            .await;
    }

    async fn handle_close(&self, message: CloseMessage) -> Result<(), Error> {
        let error = message.error.clone();
        if let Some(error) = &error {
            warn!(
                error = %error,
                allow_reconnect = ?message.allow_reconnect,
                "close message received from server"
            );
        } else {
            warn!(
                allow_reconnect = ?message.allow_reconnect,
                "close message received from server"
            );
        }

        let cancellation = error
            .clone()
            .unwrap_or_else(|| INVOCATION_CANCELED_MESSAGE.to_string());
        self.bus.cancel_all_pending(cancellation).await;

        if !self
            .transport_close_signaled
            .swap(true, Ordering::Relaxed)
        {
            let _ = self.transport_events.send(TransportEvent::Closed(
                TransportCloseInfo {
                    error,
                    allow_reconnect: message.allow_reconnect,
                },
            ));
        }

        self.transport.close().await
    }

    async fn send_completion(&self, message: CompletionMessage) -> Result<(), Error> {
        send_hub_message(
            &self.transport,
            self.protocol.as_ref(),
            self.transfer_format,
            &HubMessage::Completion(message),
        )
        .await
    }
}

pub async fn send_hub_message(
    transport: &WebSocketTransport,
    protocol: &dyn HubProtocol,
    transfer_format: TransferFormat,
    message: &HubMessage,
) -> Result<(), Error> {
    let payload = protocol
        .write_message(message)
        .map_err(|error| Error::Protocol(error.to_string()))?;

    match transfer_format {
        TransferFormat::Text => {
            let text = String::from_utf8(payload)
                .map_err(|error| Error::Protocol(format!("invalid text payload: {error}")))?;
            transport.send_text(text).await?;
        }
        TransferFormat::Binary => transport.send_binary(payload).await?,
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::message::PingMessage;

    fn test_dispatch_context(bus: MessageBus) -> DispatchContext {
        let (transport_events, _events_rx) = mpsc::unbounded_channel();
        DispatchContext {
            bus,
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(crate::hub::JsonProtocol),
            transfer_format: TransferFormat::Text,
            message_buffer: None,
            transport_events,
            transport_close_signaled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn dispatch_ping_is_noop() {
        let ctx = test_dispatch_context(MessageBus::default());
        ctx.dispatch(HubMessage::Ping(PingMessage)).await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_close_cancels_pending_and_emits_transport_event() {
        use futures_util::StreamExt;

        let bus = MessageBus::default();
        let invoke = bus.register_pending("invoke-1".into()).await.unwrap();
        let mut stream = bus.register_stream("stream-1".into()).await.unwrap();
        let (transport_events, mut events_rx) = mpsc::unbounded_channel();
        let ctx = DispatchContext {
            bus: bus.clone(),
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(crate::hub::JsonProtocol),
            transfer_format: TransferFormat::Text,
            message_buffer: None,
            transport_events,
            transport_close_signaled: Arc::new(AtomicBool::new(false)),
        };

        ctx.dispatch(HubMessage::Close(CloseMessage {
            error: Some("server shutdown".into()),
            allow_reconnect: Some(false),
        }))
        .await
        .unwrap();

        assert_eq!(invoke.await.unwrap(), Err("server shutdown".into()));
        assert_eq!(
            stream.next().await,
            Some(Err("server shutdown".into()))
        );

        let TransportEvent::Closed(info) = events_rx.try_recv().unwrap();
        assert_eq!(info.error.as_deref(), Some("server shutdown"));
        assert_eq!(info.allow_reconnect, Some(false));
    }
}
