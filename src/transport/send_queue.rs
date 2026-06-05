//! Serializes outbound transport sends and batches consecutive payloads of the same type.

use std::sync::Arc;

use futures_channel::oneshot;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::error::Error;
use crate::transport::WebSocketTransport;

enum OutboundPayload {
    Text(String),
    Binary(Vec<u8>),
}

enum QueueCommand {
    Send {
        payload: OutboundPayload,
        response: oneshot::Sender<Result<(), Error>>,
    },
    Shutdown,
}

/// Serializes transport sends so frames are written in order.
pub struct TransportSendQueue {
    commands: mpsc::UnboundedSender<QueueCommand>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl TransportSendQueue {
    pub fn new(transport: WebSocketTransport) -> Self {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(send_loop(transport, command_rx));
        Self {
            commands,
            worker: Arc::new(Mutex::new(Some(worker))),
        }
    }

    pub async fn send_text(&self, payload: impl Into<String>) -> Result<(), Error> {
        self.send(OutboundPayload::Text(payload.into())).await
    }

    pub async fn send_binary(&self, payload: Vec<u8>) -> Result<(), Error> {
        self.send(OutboundPayload::Binary(payload)).await
    }

    async fn send(&self, payload: OutboundPayload) -> Result<(), Error> {
        let (response, receiver) = oneshot::channel();
        self.commands
            .send(QueueCommand::Send { payload, response })
            .map_err(|_| Error::Protocol("transport send queue closed".into()))?;
        receiver
            .await
            .map_err(|_| Error::Protocol("transport send queue dropped response".into()))?
    }

    pub async fn stop(&self) {
        let _ = self.commands.send(QueueCommand::Shutdown);
        if let Some(worker) = self.worker.lock().await.take() {
            let _ = worker.await;
        }
    }
}

async fn send_loop(
    transport: WebSocketTransport,
    mut command_rx: mpsc::UnboundedReceiver<QueueCommand>,
) {
    let mut buffer: Vec<OutboundPayload> = Vec::new();
    let mut pending_responses: Vec<oneshot::Sender<Result<(), Error>>> = Vec::new();
    let mut wait_for_data = true;

    loop {
        if wait_for_data {
            match command_rx.recv().await {
                Some(QueueCommand::Send { payload, response }) => {
                    if let Err(error) = push_payload(&mut buffer, payload) {
                        let _ = response.send(Err(error));
                        continue;
                    }
                    pending_responses.push(response);
                    drain_ready_commands(&mut command_rx, &mut buffer, &mut pending_responses);
                    wait_for_data = false;
                }
                Some(QueueCommand::Shutdown) | None => {
                    fail_pending(
                        &mut pending_responses,
                        Error::Protocol("connection stopped".into()),
                    );
                    break;
                }
            }
        } else {
            let result = flush_buffer(&transport, &mut buffer).await;
            complete_pending(&mut pending_responses, result);
            wait_for_data = true;
        }
    }
}

fn drain_ready_commands(
    command_rx: &mut mpsc::UnboundedReceiver<QueueCommand>,
    buffer: &mut Vec<OutboundPayload>,
    pending_responses: &mut Vec<oneshot::Sender<Result<(), Error>>>,
) {
    while let Ok(command) = command_rx.try_recv() {
        match command {
            QueueCommand::Send { payload, response } => {
                if let Err(error) = push_payload(buffer, payload) {
                    let _ = response.send(Err(error));
                    continue;
                }
                pending_responses.push(response);
            }
            QueueCommand::Shutdown => {
                fail_pending(
                    pending_responses,
                    Error::Protocol("connection stopped".into()),
                );
                return;
            }
        }
    }
}

fn push_payload(buffer: &mut Vec<OutboundPayload>, payload: OutboundPayload) -> Result<(), Error> {
    if let Some(first) = buffer.first() {
        let same_type = matches!(
            (first, &payload),
            (OutboundPayload::Text(_), OutboundPayload::Text(_))
                | (OutboundPayload::Binary(_), OutboundPayload::Binary(_))
        );
        if !same_type {
            return Err(Error::Protocol(
                "transport send queue received mixed text and binary payloads".into(),
            ));
        }
    }

    buffer.push(payload);
    Ok(())
}

async fn flush_buffer(
    transport: &WebSocketTransport,
    buffer: &mut Vec<OutboundPayload>,
) -> Result<(), Error> {
    if buffer.is_empty() {
        return Ok(());
    }

    let result = match buffer.first() {
        Some(OutboundPayload::Text(_)) => {
            let payload = buffer
                .drain(..)
                .map(|entry| match entry {
                    OutboundPayload::Text(text) => text,
                    OutboundPayload::Binary(_) => unreachable!(),
                })
                .collect::<String>();
            transport.send_text_direct(payload).await
        }
        Some(OutboundPayload::Binary(_)) => {
            let payload = buffer
                .drain(..)
                .flat_map(|entry| match entry {
                    OutboundPayload::Binary(bytes) => bytes,
                    OutboundPayload::Text(_) => unreachable!(),
                })
                .collect::<Vec<u8>>();
            transport.send_binary_direct(payload).await
        }
        None => Ok(()),
    };

    if let Err(error) = &result {
        warn!(?error, "transport send queue failed to flush buffered payloads");
    }

    result
}

fn complete_pending(
    pending_responses: &mut Vec<oneshot::Sender<Result<(), Error>>>,
    result: Result<(), Error>,
) {
    let error_message = result.err().map(|error| error.to_string());
    for response in pending_responses.drain(..) {
        let _ = response.send(match &error_message {
            None => Ok(()),
            Some(message) => Err(Error::Protocol(message.clone())),
        });
    }
}

fn fail_pending(pending_responses: &mut Vec<oneshot::Sender<Result<(), Error>>>, error: Error) {
    let message = error.to_string();
    for response in pending_responses.drain(..) {
        let _ = response.send(Err(Error::Protocol(message.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn send_queue_batches_concurrent_text_payloads() {
        let (transport, outgoing) = WebSocketTransport::recording();
        let queue = Arc::new(TransportSendQueue::new(transport));

        let queue_a = queue.clone();
        let queue_b = queue.clone();
        let (first, second) = tokio::join!(queue_a.send_text("a"), queue_b.send_text("b"));
        first.unwrap();
        second.unwrap();

        let messages = outgoing.lock().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], "ab");
    }

    #[test]
    fn push_payload_rejects_mixed_payload_types() {
        let mut buffer = vec![OutboundPayload::Text("hello".into())];
        let error = push_payload(&mut buffer, OutboundPayload::Binary(vec![1, 2, 3])).unwrap_err();
        assert!(matches!(error, Error::Protocol(_)));
    }

    #[tokio::test]
    async fn stop_fails_pending_sends() {
        let (transport, _outgoing) = WebSocketTransport::recording();
        let queue = TransportSendQueue::new(transport);
        let send_count = Arc::new(AtomicUsize::new(0));
        let send_count_clone = send_count.clone();

        let queue_ref = Arc::new(queue);
        let sender = queue_ref.clone();
        let pending = tokio::spawn(async move {
            let _ = sender.send_text("pending").await;
            send_count_clone.fetch_add(1, Ordering::Relaxed);
        });

        queue_ref.stop().await;
        pending.await.unwrap();

        assert_eq!(send_count.load(Ordering::Relaxed), 1);
    }
}
