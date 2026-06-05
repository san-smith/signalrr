//! Background receive loop and keepalive for an active hub connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::bus::MessageBus;
use crate::error::Error;
use crate::hub::message::{CloseMessage, HubMessage, PingMessage};
use crate::hub::protocol::{HubProtocol, HubProtocolInput};
use crate::hub_connection::dispatch::{DispatchContext, send_hub_message};
use crate::hub_connection::message_buffer::MessageBuffer;
use crate::transport::{TransferFormat, TransportMessage, WebSocketTransport};

pub const DEFAULT_SERVER_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Event emitted when the transport closes unexpectedly.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    Closed(Option<String>),
}

/// Active hub connection runtime: transport I/O, dispatch, and keepalive tasks.
pub struct HubRuntime {
    pub transport: WebSocketTransport,
    pub hub_protocol: Arc<dyn HubProtocol>,
    pub transfer_format: TransferFormat,
    pub bus: MessageBus,
    message_buffer: Option<Arc<MessageBuffer>>,
    invocation_id: AtomicI64,
    cached_ping: Vec<u8>,
    activity: Arc<Notify>,
    shutdown_tx: watch::Sender<bool>,
    receive_task: JoinHandle<()>,
    keepalive_task: JoinHandle<()>,
}

pub struct HubRuntimeOptions {
    pub transport: WebSocketTransport,
    pub incoming: mpsc::UnboundedReceiver<TransportMessage>,
    pub hub_protocol: Arc<dyn HubProtocol>,
    pub transfer_format: TransferFormat,
    pub initial_messages: Vec<HubMessage>,
    pub server_timeout: Duration,
    pub keep_alive_interval: Duration,
    pub bus: MessageBus,
    pub message_buffer: Option<Arc<MessageBuffer>>,
    pub transport_events: mpsc::UnboundedSender<TransportEvent>,
}

impl HubRuntime {
    pub fn start(options: HubRuntimeOptions) -> Result<Self, Error> {
        let HubRuntimeOptions {
            transport,
            mut incoming,
            hub_protocol,
            transfer_format,
            initial_messages,
            server_timeout,
            keep_alive_interval,
            bus,
            message_buffer,
            transport_events,
        } = options;

        let cached_ping = hub_protocol
            .write_message(&HubMessage::Ping(PingMessage))
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let activity = Arc::new(Notify::new());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let receive_activity = activity.clone();
        let mut receive_shutdown = shutdown_rx.clone();
        let receive_protocol = hub_protocol.clone();
        let receive_transfer_format = transfer_format;
        let receive_transport = transport.clone();
        let receive_bus = bus.clone();
        let receive_buffer = message_buffer.clone();

        let receive_task = tokio::spawn(async move {
            let dispatch = DispatchContext {
                bus: receive_bus,
                transport: receive_transport.clone(),
                protocol: receive_protocol,
                transfer_format: receive_transfer_format,
                message_buffer: receive_buffer,
            };

            for message in initial_messages {
                if *receive_shutdown.borrow() {
                    return;
                }
                if dispatch.dispatch(message).await.is_err() {
                    return;
                }
                receive_activity.notify_one();
            }

            while !*receive_shutdown.borrow() {
                let message = tokio::select! {
                    _ = receive_shutdown.changed() => break,
                    message = incoming.recv() => message,
                };

                let Some(message) = message else {
                    let _ = transport_events.send(TransportEvent::Closed(None));
                    break;
                };

                receive_activity.notify_one();

                match message {
                    TransportMessage::Text(text) => {
                        if let Err(error) = process_text(&dispatch, &text).await {
                            warn!(?error, "failed to process incoming text hub message");
                        }
                    }
                    TransportMessage::Binary(bytes) => {
                        if let Err(error) = process_binary(&dispatch, &bytes).await {
                            warn!(?error, "failed to process incoming binary hub message");
                        }
                    }
                    TransportMessage::Closed(reason) => {
                        debug!(?reason, "transport closed");
                        if !*receive_shutdown.borrow() {
                            let _ = transport_events.send(TransportEvent::Closed(reason));
                        }
                        break;
                    }
                }
            }
        });

        let keepalive_transport = transport.clone();
        let keepalive_protocol = hub_protocol.clone();
        let keepalive_activity = activity.clone();
        let keepalive_shutdown = shutdown_rx;

        let keepalive_task = tokio::spawn(async move {
            run_keepalive(
                keepalive_shutdown,
                keepalive_activity,
                keepalive_transport,
                keepalive_protocol,
                transfer_format,
                server_timeout,
                keep_alive_interval,
            )
            .await;
        });

        Ok(Self {
            transport,
            hub_protocol,
            transfer_format,
            bus,
            message_buffer,
            invocation_id: AtomicI64::new(0),
            cached_ping,
            activity,
            shutdown_tx,
            receive_task,
            keepalive_task,
        })
    }

    pub fn next_invocation_id(&self) -> String {
        (self.invocation_id.fetch_add(1, Ordering::Relaxed) + 1).to_string()
    }

    pub async fn send(&self, message: &HubMessage) -> Result<(), Error> {
        if let Some(buffer) = &self.message_buffer {
            buffer
                .send(message, &self.transport, self.transfer_format)
                .await?;
        } else {
            send_hub_message(
                &self.transport,
                self.hub_protocol.as_ref(),
                self.transfer_format,
                message,
            )
            .await?;
        }
        self.activity.notify_one();
        Ok(())
    }

    pub async fn send_initial_ping(&self) -> Result<(), Error> {
        match self.transfer_format {
            TransferFormat::Text => {
                let text = String::from_utf8(self.cached_ping.clone())
                    .map_err(|error| Error::Protocol(format!("invalid ping payload: {error}")))?;
                self.transport.send_text(text).await?;
            }
            TransferFormat::Binary => {
                self.transport.send_binary(self.cached_ping.clone()).await?;
            }
        }
        self.activity.notify_one();
        Ok(())
    }

    pub async fn resend_buffered_messages(&self) -> Result<(), Error> {
        if let Some(buffer) = &self.message_buffer {
            buffer.resend(&self.transport, self.transfer_format).await?;
        }
        Ok(())
    }

    pub async fn shutdown(self) -> Result<(), Error> {
        let _ = self.shutdown_tx.send(true);

        let close_message = HubMessage::Close(CloseMessage {
            error: None,
            allow_reconnect: None,
        });
        let _ = self.send(&close_message).await;

        self.transport.close().await?;
        self.receive_task.abort();
        self.keepalive_task.abort();
        Ok(())
    }

    pub async fn abort_without_close(&self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.transport.close().await;
        self.receive_task.abort();
        self.keepalive_task.abort();
    }
}

async fn process_text(ctx: &DispatchContext, text: &str) -> Result<(), Error> {
    let messages = ctx
        .protocol
        .parse_messages(HubProtocolInput::Text(text))
        .map_err(|error| Error::Protocol(error.to_string()))?;

    for message in messages {
        ctx.dispatch(message).await?;
    }

    Ok(())
}

async fn process_binary(ctx: &DispatchContext, bytes: &[u8]) -> Result<(), Error> {
    let messages = ctx
        .protocol
        .parse_messages(HubProtocolInput::Binary(bytes))
        .map_err(|error| Error::Protocol(error.to_string()))?;

    for message in messages {
        ctx.dispatch(message).await?;
    }

    Ok(())
}

async fn run_keepalive(
    mut shutdown: watch::Receiver<bool>,
    activity: Arc<Notify>,
    transport: WebSocketTransport,
    protocol: Arc<dyn HubProtocol>,
    transfer_format: TransferFormat,
    server_timeout: Duration,
    keep_alive_interval: Duration,
) {
    let mut last_received = Instant::now();
    let mut next_ping_at = Instant::now() + keep_alive_interval;

    loop {
        if *shutdown.borrow() {
            break;
        }

        let server_deadline = last_received + server_timeout;
        let ping_deadline = next_ping_at;

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    break;
                }
            }
            _ = activity.notified() => {
                last_received = Instant::now();
                next_ping_at = Instant::now() + keep_alive_interval;
            }
            _ = tokio::time::sleep_until(server_deadline) => {
                if last_received.elapsed() >= server_timeout {
                    warn!("server timeout elapsed without receiving a message from the server");
                    let _ = transport.close().await;
                    break;
                }
            }
            _ = tokio::time::sleep_until(ping_deadline) => {
                if Instant::now() >= next_ping_at {
                    let ping = HubMessage::Ping(PingMessage);
                    let _ = send_hub_message(
                        &transport,
                        protocol.as_ref(),
                        transfer_format,
                        &ping,
                    )
                    .await;
                    next_ping_at = Instant::now() + keep_alive_interval;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::JsonProtocol;

    #[tokio::test]
    async fn runtime_generates_monotonic_invocation_ids() {
        let (transport, incoming) = WebSocketTransport::connect("ws://127.0.0.1:1", None)
            .await
            .unwrap_or((
                WebSocketTransport::disconnected(),
                mpsc::unbounded_channel().1,
            ));

        let _ = transport.close().await;
        let (transport_events, _events) = mpsc::unbounded_channel();

        let runtime = HubRuntime::start(HubRuntimeOptions {
            transport: WebSocketTransport::disconnected(),
            incoming,
            hub_protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            initial_messages: Vec::new(),
            server_timeout: DEFAULT_SERVER_TIMEOUT,
            keep_alive_interval: DEFAULT_KEEP_ALIVE_INTERVAL,
            bus: MessageBus::default(),
            message_buffer: None,
            transport_events,
        })
        .unwrap();

        assert_eq!(runtime.next_invocation_id(), "1");
        assert_eq!(runtime.next_invocation_id(), "2");
    }
}
