//! Builder for [`HubConnection`](crate::HubConnection).

use crate::hub_connection::HubConnection;
use crate::negotiate::NegotiateOptions;

/// Configures and constructs a [`HubConnection`].
#[derive(Debug, Clone)]
pub struct HubConnectionBuilder {
    url: String,
    negotiate_options: NegotiateOptions,
}

impl HubConnectionBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            negotiate_options: NegotiateOptions::new(),
        }
    }

    pub fn with_stateful_reconnect(mut self, enabled: bool) -> Self {
        self.negotiate_options.use_stateful_reconnect = enabled;
        self
    }

    pub fn build(self) -> HubConnection {
        HubConnection::new(self.url, self.negotiate_options)
    }
}

impl HubConnection {
    pub fn builder(url: impl Into<String>) -> HubConnectionBuilder {
        HubConnectionBuilder::new(url)
    }
}
