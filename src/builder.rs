//! Builder for [`HubConnection`](crate::HubConnection).

use std::sync::Arc;
use std::time::Duration;

use crate::access_token::{AccessTokenFactory, factory as access_token_factory};
use crate::handshake::HandshakeRequest;
#[cfg(feature = "messagepack-brotli")]
use crate::hub::MessagePackBrotliProtocol;
#[cfg(feature = "messagepack")]
use crate::hub::MessagePackProtocol;
use crate::hub::protocol::HubProtocol;
use crate::hub::{JsonProtocol, default_hub_protocol};
use crate::hub_connection::runtime::{DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_SERVER_TIMEOUT};
use crate::hub_connection::{
    DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE, DefaultReconnectPolicy, HubConnection,
    HubConnectionConfig, RetryPolicy,
};
use crate::negotiate::NegotiateOptions;
use crate::transport::TransferFormat;

/// Configures and constructs a [`HubConnection`].
#[derive(Clone)]
pub struct HubConnectionBuilder {
    config: HubConnectionConfig,
    handshake_timeout: Duration,
}

impl HubConnectionBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        let hub_protocol = default_hub_protocol();
        Self {
            config: HubConnectionConfig {
                base_url: url.into(),
                negotiate: NegotiateOptions::new(),
                handshake_request: HandshakeRequest {
                    protocol: hub_protocol.name().into(),
                    version: hub_protocol.version(),
                },
                handshake_version_override: None,
                transfer_format: hub_protocol.transfer_format(),
                access_token_factory: None,
                handshake_timeout: Duration::from_secs(15),
                hub_protocol,
                server_timeout: DEFAULT_SERVER_TIMEOUT,
                keep_alive_interval: DEFAULT_KEEP_ALIVE_INTERVAL,
                reconnect_policy: None,
                stateful_reconnect_buffer_size: DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE,
                http_client: None,
                log_message_content: false,
            },
            handshake_timeout: Duration::from_secs(15),
        }
    }

    pub fn with_stateful_reconnect(mut self, enabled: bool) -> Self {
        self.config.negotiate.use_stateful_reconnect = enabled;
        self
    }

    pub fn with_stateful_reconnect_buffer_size(mut self, size: usize) -> Self {
        self.config.stateful_reconnect_buffer_size = size;
        self
    }

    pub fn with_automatic_reconnect(self) -> Self {
        self.with_reconnect_policy(Arc::new(DefaultReconnectPolicy::default()))
    }

    pub fn with_reconnect_policy(mut self, policy: Arc<dyn RetryPolicy>) -> Self {
        self.config.reconnect_policy = Some(policy);
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.config
            .negotiate
            .headers
            .push((name.into(), value.into()));
        self
    }

    /// Sets an explicit handshake protocol name and version, bypassing automatic selection.
    pub fn with_handshake_protocol(mut self, protocol: impl Into<String>, version: i32) -> Self {
        self.config.handshake_request.protocol = protocol.into();
        self.config.handshake_version_override = Some(version);
        self
    }

    pub fn with_hub_protocol(mut self, protocol: Arc<dyn HubProtocol>) -> Self {
        self.config.handshake_request.protocol = protocol.name().to_string();
        self.config.handshake_version_override = None;
        self.config.transfer_format = protocol.transfer_format();
        self.config.hub_protocol = protocol;
        self
    }

    pub fn with_json_protocol(self) -> Self {
        self.with_hub_protocol(Arc::new(JsonProtocol))
    }

    #[cfg(feature = "messagepack")]
    pub fn with_messagepack_protocol(self) -> Self {
        self.with_hub_protocol(Arc::new(MessagePackProtocol))
    }

    #[cfg(feature = "messagepack-brotli")]
    pub fn with_messagepack_brotli_protocol(self) -> Self {
        self.with_hub_protocol(Arc::new(MessagePackBrotliProtocol::default()))
    }

    pub fn with_transfer_format(mut self, transfer_format: TransferFormat) -> Self {
        self.config.transfer_format = transfer_format;
        self
    }

    pub fn with_access_token_factory(mut self, factory: AccessTokenFactory) -> Self {
        self.config.access_token_factory = Some(factory);
        self
    }

    pub fn with_access_token<F, Fut>(self, callback: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, crate::Error>> + Send + 'static,
    {
        self.with_access_token_factory(access_token_factory(callback))
    }

    pub fn with_negotiate_timeout(mut self, timeout: Duration) -> Self {
        self.config.negotiate.timeout = timeout;
        self
    }

    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    pub fn with_server_timeout(mut self, timeout: Duration) -> Self {
        self.config.server_timeout = timeout;
        self
    }

    pub fn with_keep_alive_interval(mut self, interval: Duration) -> Self {
        self.config.keep_alive_interval = interval;
        self
    }

    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.config.http_client = Some(client);
        self
    }

    pub fn with_log_message_content(mut self, enabled: bool) -> Self {
        self.config.log_message_content = enabled;
        self
    }

    pub fn build(self) -> HubConnection {
        let mut config = self.config;
        config.handshake_timeout = self.handshake_timeout;
        HubConnection::new(config.into_connection_options())
    }
}

impl HubConnection {
    pub fn builder(url: impl Into<String>) -> HubConnectionBuilder {
        HubConnectionBuilder::new(url)
    }
}
