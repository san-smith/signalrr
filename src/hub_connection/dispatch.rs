//! Dispatches parsed hub messages to the message bus and client handlers.

use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use crate::bus::MessageBus;
use crate::error::Error;
use crate::hub::message::{
    CloseMessage, CompletionMessage, HubMessage, InvocationMessage, StreamItemMessage,
};
use crate::hub::protocol::HubProtocol;
use crate::transport::{TransferFormat, WebSocketTransport};

/// Shared context for inbound hub message processing.
pub struct DispatchContext {
    pub bus: MessageBus,
    pub transport: WebSocketTransport,
    pub protocol: Arc<dyn HubProtocol>,
    pub transfer_format: TransferFormat,
}

impl DispatchContext {
    pub async fn dispatch(&self, message: HubMessage) -> Result<(), Error> {
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
            HubMessage::Close(message) => {
                self.handle_close(message).await;
                Ok(())
            }
            HubMessage::StreamInvocation(_)
            | HubMessage::CancelInvocation(_)
            | HubMessage::Ack(_)
            | HubMessage::Sequence(_) => Ok(()),
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

    async fn handle_close(&self, message: CloseMessage) {
        if let Some(error) = message.error {
            warn!(error = %error, "close message received from server");
        } else {
            warn!("close message received from server");
        }
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

    #[tokio::test]
    async fn dispatch_ping_is_noop() {
        let bus = MessageBus::default();
        let ctx = DispatchContext {
            bus,
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(crate::hub::JsonProtocol),
            transfer_format: TransferFormat::Text,
        };

        ctx.dispatch(HubMessage::Ping(PingMessage)).await.unwrap();
    }
}
