//! Stateful reconnect message buffer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};

use crate::error::Error;
use crate::hub::message::{AckMessage, HubMessage, MessageType, SequenceMessage};
use crate::hub::protocol::HubProtocol;
use crate::hub_connection::dispatch::send_hub_message;
use crate::transport::{TransferFormat, WebSocketTransport};

pub const DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE: usize = 100_000;
const ACK_DELAY: Duration = Duration::from_secs(1);

struct BufferedItem {
    message: Vec<u8>,
    id: i64,
    backpressure: Option<oneshot::Sender<()>>,
}

/// Buffers invocation-related hub messages for stateful reconnect.
pub struct MessageBuffer {
    protocol: Arc<dyn HubProtocol>,
    buffer_size: usize,
    messages: Mutex<Vec<BufferedItem>>,
    total_message_count: Mutex<i64>,
    wait_for_sequence_message: AtomicBool,
    next_receiving_sequence_id: Mutex<i64>,
    latest_received_sequence_id: Arc<Mutex<i64>>,
    buffered_byte_count: Mutex<usize>,
    reconnect_in_progress: Arc<AtomicBool>,
    ack_timer: Mutex<Option<JoinHandle<()>>>,
}

impl MessageBuffer {
    pub fn new(protocol: Arc<dyn HubProtocol>, buffer_size: usize) -> Self {
        Self {
            protocol,
            buffer_size,
            messages: Mutex::new(Vec::new()),
            total_message_count: Mutex::new(0),
            wait_for_sequence_message: AtomicBool::new(false),
            next_receiving_sequence_id: Mutex::new(1),
            latest_received_sequence_id: Arc::new(Mutex::new(0)),
            buffered_byte_count: Mutex::new(0),
            reconnect_in_progress: Arc::new(AtomicBool::new(false)),
            ack_timer: Mutex::new(None),
        }
    }

    pub async fn send(
        &self,
        message: &HubMessage,
        transport: &WebSocketTransport,
        transfer_format: TransferFormat,
    ) -> Result<(), Error> {
        let serialized = self
            .protocol
            .write_message(message)
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let mut backpressure_rx = None;

        if is_invocation_message(message) {
            let mut total = self.total_message_count.lock().await;
            *total += 1;
            let id = *total;
            drop(total);

            let length = serialized.len();
            let mut buffered_byte_count = self.buffered_byte_count.lock().await;
            *buffered_byte_count += length;

            let backpressure = if *buffered_byte_count >= self.buffer_size {
                let (tx, rx) = oneshot::channel();
                backpressure_rx = Some(rx);
                Some(tx)
            } else {
                None
            };

            self.messages.lock().await.push(BufferedItem {
                message: serialized.clone(),
                id,
                backpressure,
            });
        }

        if !self.reconnect_in_progress.load(Ordering::Relaxed)
            && let Err(error) = send_serialized(transport, transfer_format, &serialized).await
        {
            self.disconnected();
            return Err(error);
        }

        if let Some(rx) = backpressure_rx {
            let _ = rx.await;
        }

        Ok(())
    }

    pub async fn ack(&self, ack: AckMessage) {
        let mut newest_acked_message = None;
        let mut messages = self.messages.lock().await;
        let mut buffered_byte_count = self.buffered_byte_count.lock().await;

        for item in messages.iter_mut() {
            if item.id <= ack.sequence_id {
                newest_acked_message = Some(item.id);
                *buffered_byte_count = buffered_byte_count.saturating_sub(item.message.len());
                if let Some(tx) = item.backpressure.take() {
                    let _ = tx.send(());
                }
            } else if *buffered_byte_count < self.buffer_size {
                if let Some(tx) = item.backpressure.take() {
                    let _ = tx.send(());
                }
            } else {
                break;
            }
        }

        if let Some(acked_id) = newest_acked_message {
            messages.retain(|item| item.id > acked_id);
        }
    }

    pub async fn should_process_message(
        &self,
        message: &HubMessage,
        transport: &WebSocketTransport,
        transfer_format: TransferFormat,
    ) -> bool {
        if self.wait_for_sequence_message.load(Ordering::Relaxed) {
            return matches!(message, HubMessage::Sequence(_));
        }

        if !is_invocation_message(message) {
            return true;
        }

        let current_id = {
            let mut next = self.next_receiving_sequence_id.lock().await;
            let id = *next;
            *next += 1;
            id
        };

        let latest = *self.latest_received_sequence_id.lock().await;
        if current_id <= latest {
            if current_id == latest {
                self.schedule_ack_timer(transport, transfer_format).await;
            }
            return false;
        }

        *self.latest_received_sequence_id.lock().await = current_id;
        self.schedule_ack_timer(transport, transfer_format).await;
        true
    }

    pub async fn reset_sequence(&self, message: SequenceMessage) -> Result<(), Error> {
        let next = *self.next_receiving_sequence_id.lock().await;
        if message.sequence_id > next {
            return Err(Error::Protocol(
                "sequence ID greater than amount of messages we've received".into(),
            ));
        }

        *self.next_receiving_sequence_id.lock().await = message.sequence_id;
        self.wait_for_sequence_message
            .store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn disconnected(&self) {
        self.reconnect_in_progress.store(true, Ordering::Relaxed);
        self.wait_for_sequence_message
            .store(true, Ordering::Relaxed);
    }

    pub async fn resend(
        &self,
        transport: &WebSocketTransport,
        transfer_format: TransferFormat,
    ) -> Result<(), Error> {
        let sequence_id = match self.messages.lock().await.first() {
            Some(item) => item.id,
            None => *self.total_message_count.lock().await + 1,
        };

        send_hub_message(
            transport,
            self.protocol.as_ref(),
            transfer_format,
            &HubMessage::Sequence(SequenceMessage { sequence_id }),
        )
        .await?;

        let messages: Vec<Vec<u8>> = self
            .messages
            .lock()
            .await
            .iter()
            .map(|item| item.message.clone())
            .collect();
        for message in messages {
            send_serialized(transport, transfer_format, &message).await?;
        }

        self.reconnect_in_progress.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub async fn dispose(&self) {
        if let Some(task) = self.ack_timer.lock().await.take() {
            task.abort();
        }
    }

    async fn schedule_ack_timer(
        &self,
        transport: &WebSocketTransport,
        transfer_format: TransferFormat,
    ) {
        if self.ack_timer.lock().await.is_some() {
            return;
        }

        let protocol = self.protocol.clone();
        let latest_received_sequence_id = self.latest_received_sequence_id.clone();
        let reconnect_in_progress = self.reconnect_in_progress.clone();
        let transport = transport.clone();

        let task = tokio::spawn(async move {
            sleep(ACK_DELAY).await;
            if reconnect_in_progress.load(Ordering::Relaxed) {
                return;
            }

            let sequence_id = *latest_received_sequence_id.lock().await;
            let _ = send_hub_message(
                &transport,
                protocol.as_ref(),
                transfer_format,
                &HubMessage::Ack(AckMessage { sequence_id }),
            )
            .await;
        });

        *self.ack_timer.lock().await = Some(task);
    }
}

pub fn is_invocation_message(message: &HubMessage) -> bool {
    matches!(
        message.message_type(),
        MessageType::Invocation
            | MessageType::StreamInvocation
            | MessageType::StreamItem
            | MessageType::Completion
            | MessageType::CancelInvocation
    )
}

async fn send_serialized(
    transport: &WebSocketTransport,
    transfer_format: TransferFormat,
    payload: &[u8],
) -> Result<(), Error> {
    match transfer_format {
        TransferFormat::Text => {
            let text = String::from_utf8(payload.to_vec())
                .map_err(|error| Error::Protocol(format!("invalid text payload: {error}")))?;
            transport.send_text(text).await
        }
        TransferFormat::Binary => transport.send_binary(payload.to_vec()).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::MessagePackProtocol;
    use crate::hub::message::{CompletionMessage, InvocationMessage, PingMessage, SequenceMessage};

    #[test]
    fn is_invocation_message_classifies_message_types() {
        assert!(is_invocation_message(&HubMessage::Invocation(
            InvocationMessage {
                headers: Default::default(),
                invocation_id: None,
                target: "t".into(),
                arguments: Vec::new(),
                stream_ids: None,
            }
        )));
        assert!(is_invocation_message(&HubMessage::Completion(
            CompletionMessage {
                headers: Default::default(),
                invocation_id: "1".into(),
                error: None,
                result: None,
            }
        )));
        assert!(!is_invocation_message(&HubMessage::Ping(PingMessage)));
    }

    #[tokio::test]
    async fn ack_removes_acked_messages() {
        let buffer = MessageBuffer::new(Arc::new(MessagePackProtocol), 100);
        let transport = WebSocketTransport::disconnected();

        buffer
            .send(
                &HubMessage::Invocation(InvocationMessage {
                    headers: Default::default(),
                    invocation_id: Some("1".into()),
                    target: "m".into(),
                    arguments: Vec::new(),
                    stream_ids: None,
                }),
                &transport,
                TransferFormat::Binary,
            )
            .await
            .unwrap();

        buffer.ack(AckMessage { sequence_id: 1 }).await;
        assert!(buffer.messages.lock().await.is_empty());
    }

    #[tokio::test]
    async fn should_process_message_waits_for_sequence_after_disconnect() {
        let buffer = MessageBuffer::new(Arc::new(MessagePackProtocol), 100);
        let transport = WebSocketTransport::disconnected();
        buffer.disconnected();

        assert!(
            !buffer
                .should_process_message(
                    &HubMessage::Invocation(InvocationMessage {
                        headers: Default::default(),
                        invocation_id: None,
                        target: "t".into(),
                        arguments: Vec::new(),
                        stream_ids: None,
                    }),
                    &transport,
                    TransferFormat::Binary,
                )
                .await
        );
        assert!(
            buffer
                .should_process_message(
                    &HubMessage::Sequence(SequenceMessage { sequence_id: 1 }),
                    &transport,
                    TransferFormat::Binary,
                )
                .await
        );
    }
}
