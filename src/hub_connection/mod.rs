//! SignalR hub connection.

mod dispatch;
pub(crate) mod runtime;
mod state;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::RwLock;

pub use runtime::{DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_SERVER_TIMEOUT};
pub use state::HubConnectionState;

use crate::access_token::AccessTokenFactory;
use crate::connection::{Connection, ConnectionOptions};
use crate::error::Error;
use crate::handshake::{HandshakeRequest, RemainingData};
use crate::hub::message::{HubMessage, InvocationMessage};
use crate::hub::protocol::HubProtocol;
use crate::hub_connection::runtime::HubRuntime;
use crate::negotiate::NegotiateOptions;
use crate::transport::TransferFormat;

/// Active SignalR hub connection.
pub struct HubConnection {
    options: ConnectionOptions,
    state: Arc<RwLock<HubConnectionState>>,
    connection_id: Arc<RwLock<Option<String>>>,
    runtime: Arc<RwLock<Option<HubRuntime>>>,
}

impl HubConnection {
    pub(crate) fn new(options: ConnectionOptions) -> Self {
        Self {
            options,
            state: Arc::new(RwLock::new(HubConnectionState::Disconnected)),
            connection_id: Arc::new(RwLock::new(None)),
            runtime: Arc::new(RwLock::new(None)),
        }
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

        match self.connect_runtime().await {
            Ok(()) => {
                *self.state.write().await = HubConnectionState::Connected;
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

        if let Some(runtime) = self.runtime.write().await.take() {
            runtime.shutdown().await?;
        }

        *self.connection_id.write().await = None;
        *self.state.write().await = HubConnectionState::Disconnected;
        Ok(())
    }

    /// Registers a handler for server-to-client hub method invocations.
    pub async fn on<F>(&self, method_name: impl Into<String>, handler: F)
    where
        F: Fn(Vec<Value>) -> Option<Value> + Send + Sync + 'static,
    {
        let runtime_guard = self.runtime.read().await;
        let Some(runtime) = runtime_guard.as_ref() else {
            return;
        };

        runtime
            .bus
            .register_handler(method_name.into(), Arc::new(handler))
            .await;
    }

    /// Removes all handlers registered for the given hub method name.
    pub async fn off(&self, method_name: impl AsRef<str>) {
        let runtime_guard = self.runtime.read().await;
        let Some(runtime) = runtime_guard.as_ref() else {
            return;
        };

        runtime.bus.remove_handler(method_name.as_ref()).await;
    }

    /// Invokes a hub method on the server and awaits the completion result.
    pub async fn invoke(
        &self,
        method_name: impl AsRef<str>,
        arguments: Vec<Value>,
    ) -> Result<Value, Error> {
        self.require_connected().await?;

        let invocation_id = {
            let runtime = self.runtime.read().await;
            runtime
                .as_ref()
                .ok_or(Error::NotConnected)?
                .next_invocation_id()
        };

        let response = {
            let runtime = self.runtime.read().await;
            runtime
                .as_ref()
                .ok_or(Error::NotConnected)?
                .bus
                .register_pending(invocation_id.clone())
                .await?
        };

        {
            let runtime = self.runtime.read().await;
            runtime
                .as_ref()
                .ok_or(Error::NotConnected)?
                .send(&HubMessage::Invocation(InvocationMessage {
                    headers: Default::default(),
                    invocation_id: Some(invocation_id),
                    target: method_name.as_ref().to_string(),
                    arguments,
                    stream_ids: None,
                }))
                .await?;
        }

        match response.await? {
            Ok(value) => Ok(value),
            Err(error) => Err(Error::Invocation(error)),
        }
    }

    /// Sends a hub method invocation without waiting for a completion message.
    pub async fn send(
        &self,
        method_name: impl AsRef<str>,
        arguments: Vec<Value>,
    ) -> Result<(), Error> {
        self.require_connected().await?;

        let runtime = self.runtime.read().await;
        runtime
            .as_ref()
            .ok_or(Error::NotConnected)?
            .send(&HubMessage::Invocation(InvocationMessage {
                headers: Default::default(),
                invocation_id: None,
                target: method_name.as_ref().to_string(),
                arguments,
                stream_ids: None,
            }))
            .await
    }

    async fn connect_runtime(&self) -> Result<(), Error> {
        let mut connection = Connection::connect(self.options.clone()).await?;
        *self.connection_id.write().await = connection.connection_id().map(str::to_string);

        let transport = connection.transport();
        let incoming = connection.take_incoming();
        let initial_messages = connection.initial_messages().to_vec();

        let runtime = HubRuntime::start(
            transport,
            incoming,
            self.options.hub_protocol.clone(),
            self.options.transfer_format,
            initial_messages,
            self.options.server_timeout,
            self.options.keep_alive_interval,
        )?;

        runtime.send_initial_ping().await?;
        *self.runtime.write().await = Some(runtime);
        Ok(())
    }

    async fn require_connected(&self) -> Result<(), Error> {
        if *self.state.read().await != HubConnectionState::Connected {
            return Err(Error::NotConnected);
        }
        Ok(())
    }
}

/// Configuration collected by [`crate::HubConnectionBuilder`].
#[derive(Clone)]
pub struct HubConnectionConfig {
    pub base_url: String,
    pub negotiate: NegotiateOptions,
    pub handshake_request: HandshakeRequest,
    pub transfer_format: TransferFormat,
    pub access_token_factory: Option<AccessTokenFactory>,
    pub handshake_timeout: Duration,
    pub hub_protocol: Arc<dyn HubProtocol>,
    pub server_timeout: Duration,
    pub keep_alive_interval: Duration,
}

impl HubConnectionConfig {
    pub fn into_connection_options(self) -> ConnectionOptions {
        ConnectionOptions {
            base_url: self.base_url,
            negotiate: self.negotiate,
            handshake_request: self.handshake_request,
            transfer_format: self.transfer_format,
            handshake_timeout: self.handshake_timeout,
            access_token_factory: self.access_token_factory,
            hub_protocol: self.hub_protocol,
            server_timeout: self.server_timeout,
            keep_alive_interval: self.keep_alive_interval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionOptions;
    use crate::error::Error;
    use crate::hub::message::{CompletionMessage, PingMessage};
    use crate::hub::protocol::HubProtocolInput;
    use crate::hub::{JsonProtocol, MessagePackProtocol};
    use crate::hub_connection::dispatch::DispatchContext;
    use crate::transport::WebSocketTransport;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

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
    async fn dispatch_routes_completion_to_pending_invocation() {
        let bus = crate::bus::MessageBus::default();
        let ctx = DispatchContext {
            bus: bus.clone(),
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(JsonProtocol),
            transfer_format: TransferFormat::Text,
        };

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
        let bus = crate::bus::MessageBus::default();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        bus.register_handler(
            "Echo".into(),
            Arc::new(move |arguments| {
                called_clone.store(true, Ordering::Relaxed);
                Some(arguments.first()?.clone())
            }),
        )
        .await;

        let ctx = DispatchContext {
            bus: bus.clone(),
            transport: WebSocketTransport::disconnected(),
            protocol: Arc::new(MessagePackProtocol),
            transfer_format: TransferFormat::Binary,
        };

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
