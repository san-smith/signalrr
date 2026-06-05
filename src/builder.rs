//! Builder for [`HubConnection`](crate::HubConnection).

use std::sync::Arc;
use std::time::Duration;

use crate::access_token::{AccessTokenFactory, factory as access_token_factory};
use crate::handshake::HandshakeRequest;
use crate::hub::protocol::HubProtocol;
use crate::hub::{JsonProtocol, MessagePackProtocol};
use crate::hub_connection::{HubConnection, HubConnectionConfig};
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
        Self {
            config: HubConnectionConfig {
                base_url: url.into(),
                negotiate: NegotiateOptions::new(),
                handshake_request: HandshakeRequest {
                    protocol: "messagepack".into(),
                    version: 2,
                },
                transfer_format: TransferFormat::Binary,
                access_token_factory: None,
                handshake_timeout: Duration::from_secs(15),
                hub_protocol: Arc::new(MessagePackProtocol),
            },
            handshake_timeout: Duration::from_secs(15),
        }
    }

    pub fn with_stateful_reconnect(mut self, enabled: bool) -> Self {
        self.config.negotiate.use_stateful_reconnect = enabled;
        if enabled {
            self.config.handshake_request.version = 2;
        }
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.config
            .negotiate
            .headers
            .push((name.into(), value.into()));
        self
    }

    pub fn with_handshake_protocol(
        mut self,
        protocol: impl Into<String>,
        version: i32,
    ) -> Self {
        self.config.handshake_request.protocol = protocol.into();
        self.config.handshake_request.version = version;
        self
    }

    pub fn with_hub_protocol(mut self, protocol: Arc<dyn HubProtocol>) -> Self {
        self.config.handshake_request = HandshakeRequest {
            protocol: protocol.name().to_string(),
            version: protocol.version(),
        };
        self.config.transfer_format = protocol.transfer_format();
        self.config.hub_protocol = protocol;
        self
    }

    pub fn with_json_protocol(self) -> Self {
        self.with_hub_protocol(Arc::new(JsonProtocol))
    }

    pub fn with_messagepack_protocol(self) -> Self {
        self.with_hub_protocol(Arc::new(MessagePackProtocol))
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
