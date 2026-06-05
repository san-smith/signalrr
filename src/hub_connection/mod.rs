//! SignalR hub connection.

mod callbacks;
mod dispatch;
pub(crate) mod message_buffer;
pub(crate) mod retry;
pub(crate) mod runtime;
mod state;
mod stream;
mod upload_stream;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{RwLock, mpsc, watch};
use tracing::{info, warn};

pub use callbacks::{CloseCallback, ReconnectedCallback, ReconnectingCallback};
pub use message_buffer::{
    DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE, MessageBuffer, is_invocation_message,
};
pub use retry::{DefaultReconnectPolicy, RetryContext, RetryPolicy};
pub use runtime::{
    DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_SERVER_TIMEOUT, HubRuntimeOptions, TransportCloseInfo,
    TransportEvent,
};
pub use state::HubConnectionState;
pub use stream::HubMethodStream;
pub use upload_stream::{ClientUploadStream, HubArgument};

use crate::access_token::AccessTokenFactory;
use crate::bus::{ClientHandlerId, ClientMethodOutput, MessageBus, INVOCATION_CANCELED_MESSAGE};
use crate::connection::{Connection, ConnectionOptions};
use crate::error::Error;
use crate::handshake::{HandshakeRequest, RemainingData};
use crate::hub::message::{
    CancelInvocationMessage, HubMessage, InvocationMessage, StreamInvocationMessage,
};
use crate::hub::protocol::HubProtocol;
use crate::hub_connection::callbacks::LifecycleCallbacks;
use crate::hub_connection::runtime::HubRuntime;
use crate::hub_connection::upload_stream::{launch_upload_streams, replace_streaming_params};
use crate::negotiate::NegotiateOptions;
use crate::transport::TransferFormat;

/// Active SignalR hub connection.
pub struct HubConnection {
    options: ConnectionOptions,
    state: Arc<RwLock<HubConnectionState>>,
    connection_id: Arc<RwLock<Option<String>>>,
    runtime: Arc<RwLock<Option<HubRuntime>>>,
    bus: MessageBus,
    message_buffer: Arc<RwLock<Option<Arc<MessageBuffer>>>>,
    reconnect_policy: Option<Arc<dyn RetryPolicy>>,
    closing: Arc<AtomicBool>,
    stop_reconnect: watch::Sender<bool>,
    supervisor: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    lifecycle: Arc<LifecycleCallbacks>,
}

impl HubConnection {
    pub(crate) fn new(options: ConnectionOptions) -> Self {
        let (stop_reconnect, _) = watch::channel(false);
        let reconnect_policy = options.reconnect_policy.clone();
        Self {
            options,
            state: Arc::new(RwLock::new(HubConnectionState::Disconnected)),
            connection_id: Arc::new(RwLock::new(None)),
            runtime: Arc::new(RwLock::new(None)),
            bus: MessageBus::default(),
            message_buffer: Arc::new(RwLock::new(None)),
            reconnect_policy,
            closing: Arc::new(AtomicBool::new(false)),
            stop_reconnect,
            supervisor: Arc::new(RwLock::new(None)),
            lifecycle: Arc::new(LifecycleCallbacks::default()),
        }
    }

    /// Registers a callback invoked when the connection has fully closed.
    pub async fn on_close<F>(&self, callback: F)
    where
        F: Fn(Option<&str>) + Send + Sync + 'static,
    {
        self.lifecycle.register_closed(Arc::new(callback)).await;
    }

    /// Registers a callback invoked when automatic reconnect begins.
    pub async fn on_reconnecting<F>(&self, callback: F)
    where
        F: Fn(Option<&str>) + Send + Sync + 'static,
    {
        self.lifecycle
            .register_reconnecting(Arc::new(callback))
            .await;
    }

    /// Registers a callback invoked after a successful automatic reconnect.
    pub async fn on_reconnected<F>(&self, callback: F)
    where
        F: Fn(Option<&str>) + Send + Sync + 'static,
    {
        self.lifecycle
            .register_reconnected(Arc::new(callback))
            .await;
    }

    /// Hub endpoint URL configured on the builder.
    pub fn url(&self) -> &str {
        &self.options.base_url
    }

    /// Current connection state.
    pub async fn state(&self) -> HubConnectionState {
        *self.state.read().await
    }

    /// Server-assigned connection identifier, if negotiate returned one.
    pub async fn connection_id(&self) -> Option<String> {
        self.connection_id.read().await.clone()
    }

    /// Remaining transport data received together with the handshake response.
    ///
    /// Available only immediately after [`Self::start`] before any further reads.
    pub fn remaining_handshake_data(&self) -> Option<&RemainingData> {
        None
    }

    /// Hub messages parsed from data received together with the handshake response.
    pub async fn initial_messages(&self) -> Vec<HubMessage> {
        self.runtime
            .read()
            .await
            .as_ref()
            .map(|_| Vec::new())
            .unwrap_or_default()
    }

    /// Negotiates, opens the WebSocket transport, and completes the handshake.
    pub async fn start(&self) -> Result<(), Error> {
        {
            let state = self.state.read().await;
            if *state != HubConnectionState::Disconnected {
                return Err(Error::InvalidState(format!(
                    "cannot start a HubConnection in the '{state:?}' state"
                )));
            }
        }

        *self.state.write().await = HubConnectionState::Connecting;
        self.closing.store(false, Ordering::Relaxed);
        let _ = self.stop_reconnect.send(false);

        if self.options.negotiate.use_stateful_reconnect
            && self.message_buffer.read().await.is_none()
        {
            *self.message_buffer.write().await = Some(Arc::new(MessageBuffer::new(
                self.options.hub_protocol.clone(),
                self.options.stateful_reconnect_buffer_size,
            )));
        }

        match self.connect_runtime(false).await {
            Ok(events_rx) => {
                *self.state.write().await = HubConnectionState::Connected;
                self.lifecycle.mark_started();
                self.spawn_supervisor(events_rx).await;
                Ok(())
            }
            Err(error) => {
                *self.runtime.write().await = None;
                *self.connection_id.write().await = None;
                *self.state.write().await = HubConnectionState::Disconnected;
                Err(error)
            }
        }
    }

    /// Closes the underlying transport connection.
    pub async fn close(&self) -> Result<(), Error> {
        let current_state = *self.state.read().await;
        if current_state == HubConnectionState::Disconnected {
            return Ok(());
        }

        if current_state == HubConnectionState::Disconnecting {
            return Ok(());
        }

        *self.state.write().await = HubConnectionState::Disconnecting;
        self.closing.store(true, Ordering::Relaxed);
        let _ = self.stop_reconnect.send(true);

        if let Some(task) = self.supervisor.write().await.take() {
            task.abort();
        }

        if let Some(buffer) = self.message_buffer.read().await.as_ref() {
            buffer.dispose().await;
        }

        self.bus
            .cancel_all_pending(INVOCATION_CANCELED_MESSAGE)
            .await;

        if let Some(runtime) = self.runtime.write().await.take() {
            runtime.shutdown().await?;
        }

        *self.message_buffer.write().await = None;

        *self.connection_id.write().await = None;
        *self.state.write().await = HubConnectionState::Disconnected;
        self.lifecycle.invoke_closed(None).await;
        Ok(())
    }

    /// Registers a synchronous handler for server-to-client hub method invocations.
    ///
    /// Returns a [`ClientHandlerId`] that can be passed to [`Self::off_handler`].
    pub async fn on<F>(&self, method_name: impl Into<String>, handler: F) -> ClientHandlerId
    where
        F: Fn(Vec<Value>) -> Option<Value> + Send + Sync + 'static,
    {
        self.bus
            .register_handler(
                method_name.into(),
                Arc::new(move |args| ClientMethodOutput::ready(Ok(handler(args)))),
            )
            .await
    }

    /// Registers an asynchronous handler for server-to-client hub method invocations.
    ///
    /// The handler may return [`Ok(None)`] when no result should be sent to the server,
    /// or [`Err`] when the invocation should complete with an error.
    pub async fn on_async<F, Fut>(
        &self,
        method_name: impl Into<String>,
        handler: F,
    ) -> ClientHandlerId
    where
        F: Fn(Vec<Value>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<Value>, String>> + Send + 'static,
    {
        self.bus
            .register_handler(
                method_name.into(),
                Arc::new(move |args| ClientMethodOutput::r#async(handler(args))),
            )
            .await
    }

    /// Removes all handlers registered for the given hub method name.
    pub async fn off(&self, method_name: impl AsRef<str>) {
        self.bus
            .remove_handler(method_name.as_ref(), None)
            .await;
    }

    /// Removes a single handler previously registered with [`Self::on`] or [`Self::on_async`].
    pub async fn off_handler(
        &self,
        method_name: impl AsRef<str>,
        handler_id: ClientHandlerId,
    ) {
        self.bus
            .remove_handler(method_name.as_ref(), Some(handler_id))
            .await;
    }

    /// Invokes a hub method on the server and awaits the completion result.
    ///
    /// Arguments may include [`ClientUploadStream`] values via [`HubArgument::Stream`].
    pub async fn invoke(
        &self,
        method_name: impl AsRef<str>,
        arguments: Vec<HubArgument>,
    ) -> Result<Value, Error> {
        self.require_connected().await?;

        let runtime = self.runtime.clone();
        let (arguments, stream_ids, streams) = {
            let active_runtime = runtime.read().await;
            let active_runtime = active_runtime.as_ref().ok_or(Error::NotConnected)?;
            replace_streaming_params(arguments, || active_runtime.next_invocation_id())
        };

        let invocation_id = {
            let active_runtime = runtime.read().await;
            active_runtime
                .as_ref()
                .ok_or(Error::NotConnected)?
                .next_invocation_id()
        };

        let response = self.bus.register_pending(invocation_id.clone()).await?;

        let message = HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: Some(invocation_id),
            target: method_name.as_ref().to_string(),
            arguments,
            stream_ids,
        });

        launch_upload_streams(
            runtime.clone(),
            streams,
            async move {
                let active_runtime = runtime.read().await;
                active_runtime
                    .as_ref()
                    .ok_or(Error::NotConnected)?
                    .send(&message)
                    .await
            },
        );

        match response.await? {
            Ok(value) => Ok(value),
            Err(error) => Err(Error::Invocation(error)),
        }
    }

    /// Invokes a streaming hub method and returns items until the server completes the stream.
    ///
    /// Arguments may include [`ClientUploadStream`] values via [`HubArgument::Stream`].
    pub async fn stream(
        &self,
        method_name: impl AsRef<str>,
        arguments: Vec<HubArgument>,
    ) -> Result<HubMethodStream, Error> {
        self.require_connected().await?;

        let runtime = self.runtime.clone();
        let (arguments, stream_ids, streams) = {
            let active_runtime = runtime.read().await;
            let active_runtime = active_runtime.as_ref().ok_or(Error::NotConnected)?;
            replace_streaming_params(arguments, || active_runtime.next_invocation_id())
        };

        let invocation_id = {
            let active_runtime = runtime.read().await;
            active_runtime
                .as_ref()
                .ok_or(Error::NotConnected)?
                .next_invocation_id()
        };

        let items = self.bus.register_stream(invocation_id.clone()).await?;
        let stream = HubMethodStream::new(
            invocation_id.clone(),
            items,
            self.runtime.clone(),
            self.bus.clone(),
        );

        let message = HubMessage::StreamInvocation(StreamInvocationMessage {
            headers: Default::default(),
            invocation_id,
            target: method_name.as_ref().to_string(),
            arguments,
            stream_ids,
        });

        launch_upload_streams(
            runtime.clone(),
            streams,
            async move {
                let active_runtime = runtime.read().await;
                active_runtime
                    .as_ref()
                    .ok_or(Error::NotConnected)?
                    .send(&message)
                    .await
            },
        );

        Ok(stream)
    }

    /// Cancels an active streaming hub invocation by id.
    pub async fn cancel_stream(&self, invocation_id: impl AsRef<str>) -> Result<(), Error> {
        self.require_connected().await?;
        self.bus.remove_pending(invocation_id.as_ref()).await;

        let runtime = self.runtime.read().await;
        runtime
            .as_ref()
            .ok_or(Error::NotConnected)?
            .send(&HubMessage::CancelInvocation(CancelInvocationMessage {
                headers: Default::default(),
                invocation_id: invocation_id.as_ref().to_string(),
            }))
            .await
    }

    /// Sends a hub method invocation without waiting for a completion message.
    ///
    /// Arguments may include [`ClientUploadStream`] values via [`HubArgument::Stream`].
    pub async fn send(
        &self,
        method_name: impl AsRef<str>,
        arguments: Vec<HubArgument>,
    ) -> Result<(), Error> {
        self.require_connected().await?;

        let runtime = self.runtime.clone();
        let (arguments, stream_ids, streams) = {
            let active_runtime = runtime.read().await;
            let active_runtime = active_runtime.as_ref().ok_or(Error::NotConnected)?;
            replace_streaming_params(arguments, || active_runtime.next_invocation_id())
        };

        let message = HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: None,
            target: method_name.as_ref().to_string(),
            arguments,
            stream_ids,
        });

        launch_upload_streams(
            runtime.clone(),
            streams,
            async move {
                let active_runtime = runtime.read().await;
                active_runtime
                    .as_ref()
                    .ok_or(Error::NotConnected)?
                    .send(&message)
                    .await
            },
        );

        Ok(())
    }

    async fn connect_runtime(
        &self,
        is_reconnect: bool,
    ) -> Result<mpsc::UnboundedReceiver<TransportEvent>, Error> {
        let mut connection = Connection::connect(self.options.clone()).await?;
        *self.connection_id.write().await = connection.connection_id().map(str::to_string);

        let transport = connection.transport();
        let incoming = connection.take_incoming();
        let initial_messages = connection.initial_messages().to_vec();
        let (transport_events, events_rx) = mpsc::unbounded_channel();

        let runtime = HubRuntime::start(HubRuntimeOptions {
            transport,
            incoming,
            hub_protocol: self.options.hub_protocol.clone(),
            transfer_format: self.options.transfer_format,
            initial_messages,
            server_timeout: self.options.server_timeout,
            keep_alive_interval: self.options.keep_alive_interval,
            bus: self.bus.clone(),
            message_buffer: self.message_buffer.read().await.clone(),
            transport_events,
        })?;

        runtime.send_initial_ping().await?;

        if is_reconnect {
            runtime.resend_buffered_messages().await?;
        }

        *self.runtime.write().await = Some(runtime);
        Ok(events_rx)
    }

    async fn spawn_supervisor(&self, mut events_rx: mpsc::UnboundedReceiver<TransportEvent>) {
        if let Some(task) = self.supervisor.write().await.take() {
            task.abort();
        }

        let state = self.state.clone();
        let runtime = self.runtime.clone();
        let connection_id = self.connection_id.clone();
        let options = self.options.clone();
        let bus = self.bus.clone();
        let message_buffer = self.message_buffer.clone();
        let reconnect_policy = self.reconnect_policy.clone();
        let closing = self.closing.clone();
        let lifecycle = self.lifecycle.clone();
        let mut stop_reconnect = self.stop_reconnect.subscribe();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = stop_reconnect.changed() => {
                        if changed.is_ok() && *stop_reconnect.borrow() {
                            break;
                        }
                    }
                    event = events_rx.recv() => {
                        let Some(TransportEvent::Closed(info)) = event else {
                            break;
                        };

                        if closing.load(Ordering::Relaxed) {
                            break;
                        }

                        let cancellation = info
                            .error
                            .clone()
                            .unwrap_or_else(|| INVOCATION_CANCELED_MESSAGE.to_string());
                        bus.cancel_all_pending(cancellation).await;

                        let close_reason = info.error.as_deref();

                        let Some(policy) = reconnect_policy.clone() else {
                            *state.write().await = HubConnectionState::Disconnected;
                            lifecycle.invoke_closed(close_reason).await;
                            break;
                        };

                        if info.allow_reconnect == Some(false) {
                            *state.write().await = HubConnectionState::Disconnected;
                            lifecycle.invoke_closed(close_reason).await;
                            break;
                        }

                        warn!(?info.error, "hub transport closed, attempting reconnect");
                        if let Some(buffer) = message_buffer.read().await.as_ref() {
                            buffer.disconnected();
                        }

                        if let Some(current) = runtime.write().await.take() {
                            current.abort_without_close().await;
                        }

                        let started = Instant::now();
                        let mut attempt = 0usize;
                        let mut retry_reason = info
                            .error
                            .clone()
                            .unwrap_or_else(|| "connection closed".into());

                        let Some(initial_delay) = policy.next_retry_delay(&RetryContext {
                            elapsed: started.elapsed(),
                            previous_retry_count: attempt,
                            retry_reason: retry_reason.clone(),
                        }) else {
                            *state.write().await = HubConnectionState::Disconnected;
                            lifecycle.invoke_closed(Some(&retry_reason)).await;
                            break;
                        };

                        *state.write().await = HubConnectionState::Reconnecting;
                        lifecycle
                            .invoke_reconnecting(close_reason)
                            .await;

                        if closing.load(Ordering::Relaxed)
                            || *state.read().await != HubConnectionState::Reconnecting
                        {
                            break;
                        }

                        attempt += 1;
                        info!(?initial_delay, attempt, "waiting before reconnect attempt");
                        tokio::time::sleep(initial_delay).await;

                        loop {
                            if closing.load(Ordering::Relaxed) || *stop_reconnect.borrow() {
                                break;
                            }

                            if *state.read().await != HubConnectionState::Reconnecting {
                                break;
                            }

                            match reconnect_once(
                                &options,
                                &bus,
                                &message_buffer,
                                &runtime,
                                &connection_id,
                            )
                            .await
                            {
                                Ok(new_events_rx) => {
                                    *state.write().await = HubConnectionState::Connected;
                                    let reconnected_id = connection_id.read().await.clone();
                                    info!(connection_id = ?reconnected_id, "hub reconnected");
                                    lifecycle
                                        .invoke_reconnected(reconnected_id.as_deref())
                                        .await;
                                    events_rx = new_events_rx;
                                    break;
                                }
                                Err(error) => {
                                    retry_reason = error.to_string();
                                }
                            }

                            let Some(delay) = policy.next_retry_delay(&RetryContext {
                                elapsed: started.elapsed(),
                                previous_retry_count: attempt,
                                retry_reason: retry_reason.clone(),
                            }) else {
                                *state.write().await = HubConnectionState::Disconnected;
                                lifecycle.invoke_closed(Some(&retry_reason)).await;
                                return;
                            };

                            attempt += 1;
                            info!(?delay, attempt, "waiting before reconnect attempt");
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        });

        *self.supervisor.write().await = Some(task);
    }

    async fn require_connected(&self) -> Result<(), Error> {
        if *self.state.read().await != HubConnectionState::Connected {
            return Err(Error::NotConnected);
        }
        Ok(())
    }
}

async fn reconnect_once(
    options: &ConnectionOptions,
    bus: &MessageBus,
    message_buffer: &Arc<RwLock<Option<Arc<MessageBuffer>>>>,
    runtime: &Arc<RwLock<Option<HubRuntime>>>,
    connection_id: &Arc<RwLock<Option<String>>>,
) -> Result<mpsc::UnboundedReceiver<TransportEvent>, Error> {
    let mut connection = Connection::connect(options.clone()).await?;
    *connection_id.write().await = connection.connection_id().map(str::to_string);

    let transport = connection.transport();
    let incoming = connection.take_incoming();
    let initial_messages = connection.initial_messages().to_vec();
    let (transport_events, events_rx) = mpsc::unbounded_channel();

    let hub_runtime = HubRuntime::start(HubRuntimeOptions {
        transport,
        incoming,
        hub_protocol: options.hub_protocol.clone(),
        transfer_format: options.transfer_format,
        initial_messages,
        server_timeout: options.server_timeout,
        keep_alive_interval: options.keep_alive_interval,
        bus: bus.clone(),
        message_buffer: message_buffer.read().await.clone(),
        transport_events,
    })?;

    hub_runtime.send_initial_ping().await?;
    hub_runtime.resend_buffered_messages().await?;
    *runtime.write().await = Some(hub_runtime);
    Ok(events_rx)
}

/// Configuration collected by [`crate::HubConnectionBuilder`].
#[derive(Clone)]
pub struct HubConnectionConfig {
    pub base_url: String,
    pub negotiate: NegotiateOptions,
    pub handshake_request: HandshakeRequest,
    pub handshake_version_override: Option<i32>,
    pub transfer_format: TransferFormat,
    pub access_token_factory: Option<AccessTokenFactory>,
    pub handshake_timeout: Duration,
    pub hub_protocol: Arc<dyn HubProtocol>,
    pub server_timeout: Duration,
    pub keep_alive_interval: Duration,
    pub reconnect_policy: Option<Arc<dyn RetryPolicy>>,
    pub stateful_reconnect_buffer_size: usize,
}

impl HubConnectionConfig {
    pub fn into_connection_options(self) -> ConnectionOptions {
        ConnectionOptions {
            base_url: self.base_url,
            negotiate: self.negotiate,
            handshake_request: self.handshake_request,
            handshake_version_override: self.handshake_version_override,
            transfer_format: self.transfer_format,
            handshake_timeout: self.handshake_timeout,
            access_token_factory: self.access_token_factory,
            hub_protocol: self.hub_protocol,
            server_timeout: self.server_timeout,
            keep_alive_interval: self.keep_alive_interval,
            reconnect_policy: self.reconnect_policy,
            stateful_reconnect_buffer_size: self.stateful_reconnect_buffer_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionOptions;
    use crate::error::Error;
    use crate::hub::message::{CompletionMessage, PingMessage, StreamItemMessage};
    use crate::hub::protocol::HubProtocolInput;
    use crate::hub::{JsonProtocol, MessagePackProtocol};
    use crate::hub_connection::dispatch::DispatchContext;
    use crate::transport::WebSocketTransport;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn test_dispatch_context(bus: crate::bus::MessageBus) -> DispatchContext {
        let (transport_events, _events_rx) = mpsc::unbounded_channel();
        DispatchContext {
            bus,
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            message_buffer: None,
            transport_events,
            transport_close_signaled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn start_requires_disconnected_state() {
        let connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        *connection.state.write().await = HubConnectionState::Connected;

        let error = connection
            .start()
            .await
            .expect_err("start should fail when already connected");

        assert!(matches!(error, Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn close_is_idempotent_when_disconnected() {
        let connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        connection.close().await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(connection.state().await, HubConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn invoke_requires_connected_state() {
        let connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        let error = connection
            .invoke("Method", Vec::new())
            .await
            .expect_err("invoke should fail when disconnected");

        assert!(matches!(error, Error::NotConnected));
    }

    #[tokio::test]
    async fn stream_requires_connected_state() {
        let connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        let error = connection.stream("Method", Vec::new()).await;
        assert!(matches!(error, Err(Error::NotConnected)));
    }

    #[tokio::test]
    async fn dispatch_routes_stream_items_until_completion() {
        use futures_util::StreamExt;
        use tokio::sync::RwLock;

        let bus = crate::bus::MessageBus::default();
        let id = "stream-42".to_string();
        let items = bus.register_stream(id.clone()).await.unwrap();
        let mut stream =
            HubMethodStream::new(id.clone(), items, Arc::new(RwLock::new(None)), bus.clone());

        let ctx = test_dispatch_context(bus.clone());

        ctx.dispatch(HubMessage::StreamItem(StreamItemMessage {
            headers: Default::default(),
            invocation_id: Some(id.clone()),
            item: Value::String("chunk".into()),
        }))
        .await
        .unwrap();

        let _ = ctx
            .dispatch(HubMessage::Completion(CompletionMessage {
                headers: Default::default(),
                invocation_id: id,
                error: None,
                result: None,
            }))
            .await;

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Value::String("chunk".into())
        );
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn dispatch_routes_completion_to_pending_invocation() {
        let bus = crate::bus::MessageBus::default();
        let ctx = test_dispatch_context(bus.clone());

        let response = bus.register_pending("42".into()).await.unwrap();
        ctx.dispatch(HubMessage::Completion(CompletionMessage {
            headers: Default::default(),
            invocation_id: "42".into(),
            error: None,
            result: Some(Value::String("ok".into())),
        }))
        .await
        .unwrap();

        assert_eq!(response.await.unwrap(), Ok(Value::String("ok".into())));
    }

    #[tokio::test]
    async fn dispatch_invokes_registered_client_handler() {
        use crate::bus::ClientMethodOutput;

        let bus = crate::bus::MessageBus::default();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        bus.register_handler(
            "Echo".into(),
            Arc::new(move |arguments| {
                called_clone.store(true, Ordering::Relaxed);
                ClientMethodOutput::ready(Ok(arguments.first().cloned()))
            }),
        )
        .await;

        let ctx = test_dispatch_context(bus.clone());

        ctx.dispatch(HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: Some("1".into()),
            target: "Echo".into(),
            arguments: vec![Value::String("hello".into())],
            stream_ids: None,
        }))
        .await
        .unwrap();

        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn dispatch_invokes_all_registered_client_handlers() {
        use crate::bus::ClientMethodOutput;

        let bus = crate::bus::MessageBus::default();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));

        bus.register_handler(
            "Notify".into(),
            Arc::new({
                let first = first.clone();
                move |_| {
                    first.store(true, Ordering::Relaxed);
                    ClientMethodOutput::ready(Ok(None))
                }
            }),
        )
        .await;
        bus.register_handler(
            "Notify".into(),
            Arc::new({
                let second = second.clone();
                move |_| {
                    second.store(true, Ordering::Relaxed);
                    ClientMethodOutput::ready(Ok(None))
                }
            }),
        )
        .await;

        let ctx = test_dispatch_context(bus);
        ctx.dispatch(HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: None,
            target: "Notify".into(),
            arguments: vec![],
            stream_ids: None,
        }))
        .await
        .unwrap();

        assert!(first.load(Ordering::Relaxed));
        assert!(second.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn dispatch_multiple_handler_results_send_error_completion() {
        use crate::bus::ClientMethodOutput;

        let bus = crate::bus::MessageBus::default();
        bus.register_handler(
            "Echo".into(),
            Arc::new(|_| ClientMethodOutput::ready(Ok(Some(Value::String("one".into()))))),
        )
        .await;
        bus.register_handler(
            "Echo".into(),
            Arc::new(|_| ClientMethodOutput::ready(Ok(Some(Value::String("two".into()))))),
        )
        .await;

        let (transport, outgoing) = WebSocketTransport::recording();
        let ctx = DispatchContext {
            bus,
            transport,
            protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            message_buffer: None,
            transport_events: mpsc::unbounded_channel().0,
            transport_close_signaled: Arc::new(AtomicBool::new(false)),
        };

        ctx.dispatch(HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: Some("1".into()),
            target: "Echo".into(),
            arguments: vec![],
            stream_ids: None,
        }))
        .await
        .unwrap();

        let text = outgoing.lock().await.pop().unwrap();
        assert!(text.contains("Client provided multiple results."));
    }

    #[tokio::test]
    async fn dispatch_async_handler_can_complete_with_result() {
        use crate::bus::ClientMethodOutput;

        let bus = crate::bus::MessageBus::default();
        bus.register_handler(
            "Echo".into(),
            Arc::new(|_| {
                ClientMethodOutput::r#async(async {
                    tokio::task::yield_now().await;
                    Ok(Some(Value::String("async".into())))
                })
            }),
        )
        .await;

        let (transport, outgoing) = WebSocketTransport::recording();
        let ctx = DispatchContext {
            bus,
            transport,
            protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
            message_buffer: None,
            transport_events: mpsc::unbounded_channel().0,
            transport_close_signaled: Arc::new(AtomicBool::new(false)),
        };

        ctx.dispatch(HubMessage::Invocation(InvocationMessage {
            headers: Default::default(),
            invocation_id: Some("1".into()),
            target: "Echo".into(),
            arguments: vec![],
            stream_ids: None,
        }))
        .await
        .unwrap();

        let text = outgoing.lock().await.pop().unwrap();
        assert!(text.contains("async"));
    }

    #[test]
    fn json_protocol_round_trip_through_dispatch_context() {
        let original = HubMessage::Completion(CompletionMessage {
            headers: Default::default(),
            invocation_id: "abc".into(),
            error: None,
            result: Some(Value::Number(1514805840000_i64.into())),
        });

        let encoded = JsonProtocol.write_message(&original).unwrap();
        let decoded = JsonProtocol
            .parse_messages(HubProtocolInput::Text(
                std::str::from_utf8(&encoded).unwrap(),
            ))
            .unwrap();

        assert_eq!(decoded, vec![original]);
    }

    #[test]
    fn messagepack_protocol_round_trip_ping() {
        let original = HubMessage::Ping(PingMessage);
        let encoded = MessagePackProtocol.write_message(&original).unwrap();
        let decoded = MessagePackProtocol
            .parse_messages(HubProtocolInput::Binary(&encoded))
            .unwrap();
        assert_eq!(decoded, vec![original]);
    }
}
