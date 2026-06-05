//! SignalR hub connection.

mod state;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

pub use state::HubConnectionState;

use crate::access_token::AccessTokenFactory;
use crate::connection::{Connection, ConnectionOptions};
use crate::error::Error;
use crate::handshake::{HandshakeRequest, RemainingData};
use crate::negotiate::NegotiateOptions;
use crate::transport::TransferFormat;

/// Active SignalR hub connection.
pub struct HubConnection {
    options: ConnectionOptions,
    state: Arc<RwLock<HubConnectionState>>,
    connection: Option<Connection>,
}

impl HubConnection {
    pub(crate) fn new(options: ConnectionOptions) -> Self {
        Self {
            options,
            state: Arc::new(RwLock::new(HubConnectionState::Disconnected)),
            connection: None,
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
    pub fn connection_id(&self) -> Option<&str> {
        self.connection.as_ref()?.connection_id()
    }

    /// Remaining transport data received together with the handshake response.
    pub fn remaining_handshake_data(&self) -> Option<&RemainingData> {
        self.connection
            .as_ref()
            .and_then(|connection| connection.remaining_handshake_data())
    }

    /// Negotiates, opens the WebSocket transport, and completes the handshake.
    pub async fn start(&mut self) -> Result<(), Error> {
        {
            let state = self.state.read().await;
            if *state != HubConnectionState::Disconnected {
                return Err(Error::InvalidState(format!(
                    "cannot start a HubConnection in the '{state:?}' state"
                )));
            }
        }

        *self.state.write().await = HubConnectionState::Connecting;

        match Connection::connect(self.options.clone()).await {
            Ok(connection) => {
                self.connection = Some(connection);
                *self.state.write().await = HubConnectionState::Connected;
                Ok(())
            }
            Err(error) => {
                *self.state.write().await = HubConnectionState::Disconnected;
                Err(error)
            }
        }
    }

    /// Closes the underlying transport connection.
    pub async fn close(&mut self) -> Result<(), Error> {
        let current_state = *self.state.read().await;
        if current_state == HubConnectionState::Disconnected {
            return Ok(());
        }

        if current_state == HubConnectionState::Disconnecting {
            return Ok(());
        }

        *self.state.write().await = HubConnectionState::Disconnecting;

        if let Some(connection) = self.connection.take() {
            connection.close().await?;
        }

        *self.state.write().await = HubConnectionState::Disconnected;
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectionOptions;
    use crate::error::Error;

    #[tokio::test]
    async fn start_requires_disconnected_state() {
        let mut connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        *connection.state.write().await = HubConnectionState::Connected;

        let error = connection
            .start()
            .await
            .expect_err("start should fail when already connected");

        assert!(matches!(error, Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn close_is_idempotent_when_disconnected() {
        let mut connection = HubConnection::new(ConnectionOptions::new("https://example.com/signalr"));
        connection.close().await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(connection.state().await, HubConnectionState::Disconnected);
    }
}
