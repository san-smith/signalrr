//! Internal SignalR connection: negotiate, WebSocket, and handshake.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time;
use tracing::{debug, info};

use crate::access_token::AccessTokenFactory;
use crate::error::Error;
use crate::handshake::{
    HandshakeInput, HandshakeProtocol, HandshakeRequest, HandshakeResult, RemainingData,
    resolve_handshake_version,
};
use crate::hub::message::HubMessage;
use crate::hub::protocol::{HubProtocol, HubProtocolInput};
use crate::hub_connection::DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE;
use crate::hub_connection::retry::RetryPolicy;
use crate::hub_connection::runtime::{DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_SERVER_TIMEOUT};
use crate::negotiate::{NegotiateOptions, NegotiateResult, build_websocket_url, negotiate};
use crate::transport::{TransferFormat, TransportMessage, WebSocketTransport};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Configuration for establishing a transport connection.
#[derive(Clone)]
pub struct ConnectionOptions {
    pub base_url: String,
    pub negotiate: NegotiateOptions,
    pub handshake_request: HandshakeRequest,
    pub handshake_version_override: Option<i32>,
    pub transfer_format: TransferFormat,
    pub handshake_timeout: Duration,
    pub access_token_factory: Option<AccessTokenFactory>,
    pub hub_protocol: Arc<dyn HubProtocol>,
    pub server_timeout: Duration,
    pub keep_alive_interval: Duration,
    pub reconnect_policy: Option<Arc<dyn RetryPolicy>>,
    pub stateful_reconnect_buffer_size: usize,
}

impl ConnectionOptions {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            negotiate: NegotiateOptions::new(),
            handshake_request: HandshakeRequest {
                protocol: "messagepack".into(),
                version: 2,
            },
            handshake_version_override: None,
            transfer_format: TransferFormat::Binary,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            access_token_factory: None,
            hub_protocol: Arc::new(crate::hub::MessagePackProtocol),
            server_timeout: DEFAULT_SERVER_TIMEOUT,
            keep_alive_interval: DEFAULT_KEEP_ALIVE_INTERVAL,
            reconnect_policy: None,
            stateful_reconnect_buffer_size: DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE,
        }
    }
}

/// Established SignalR transport connection.
#[allow(dead_code)]
pub struct Connection {
    base_url: String,
    connection_id: Option<String>,
    connection_token: String,
    transport: WebSocketTransport,
    handshake_result: HandshakeResult,
    initial_messages: Vec<HubMessage>,
    incoming: mpsc::UnboundedReceiver<TransportMessage>,
}

impl Connection {
    /// Negotiates, opens the WebSocket, and completes the SignalR handshake.
    pub async fn connect(options: ConnectionOptions) -> Result<Self, Error> {
        let access_token = resolve_access_token(options.access_token_factory.as_ref()).await?;

        let NegotiateResult {
            response,
            access_token: negotiate_access_token,
        } = negotiate(
            &options.base_url,
            &options.negotiate,
            access_token.as_deref(),
        )
        .await?;

        let effective_access_token = negotiate_access_token.or(access_token);
        let connection_token = response.websocket_token()?.to_string();
        let connection_id = response.connection_id.clone();
        let websocket_url = build_websocket_url(
            &options.base_url,
            &connection_token,
            effective_access_token.as_deref(),
        )?;

        debug!(url = %websocket_url, "starting websocket transport");
        let (transport, mut incoming) =
            WebSocketTransport::connect(&websocket_url, effective_access_token.as_deref()).await?;

        let handshake_request = HandshakeRequest {
            protocol: options.handshake_request.protocol.clone(),
            version: resolve_handshake_version(
                options.negotiate.use_stateful_reconnect,
                options.hub_protocol.version(),
                options.handshake_version_override,
            ),
        };

        let handshake_payload = HandshakeProtocol.write_request(&handshake_request)?;
        transport.send_text(handshake_payload).await?;

        let handshake_message = time::timeout(options.handshake_timeout, incoming.recv())
            .await
            .map_err(|_| Error::Handshake("handshake timeout".into()))?
            .ok_or_else(|| Error::Handshake("connection closed during handshake".into()))?;

        let handshake_result = parse_transport_handshake(handshake_message)?;
        if let Some(error) = &handshake_result.response.error {
            return Err(Error::Handshake(error.clone()));
        }

        let initial_messages = parse_initial_messages(&options.hub_protocol, &handshake_result)?;

        info!(
            connection_id = connection_id.as_deref().unwrap_or("unknown"),
            protocol = %handshake_request.protocol,
            version = handshake_request.version,
            "signalr handshake completed"
        );

        Ok(Self {
            base_url: options.base_url,
            connection_id,
            connection_token,
            transport,
            handshake_result,
            initial_messages,
            incoming,
        })
    }

    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    pub fn remaining_handshake_data(&self) -> Option<&RemainingData> {
        self.handshake_result.remaining.as_ref()
    }

    pub fn initial_messages(&self) -> &[HubMessage] {
        &self.initial_messages
    }

    pub fn transport(&self) -> WebSocketTransport {
        self.transport.clone()
    }

    pub fn take_incoming(&mut self) -> mpsc::UnboundedReceiver<TransportMessage> {
        std::mem::replace(&mut self.incoming, mpsc::unbounded_channel().1)
    }

    pub async fn close(self) -> Result<(), Error> {
        self.transport.close().await
    }
}

#[allow(dead_code)]
impl Connection {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn connection_token(&self) -> &str {
        &self.connection_token
    }

    pub fn handshake_result(&self) -> &HandshakeResult {
        &self.handshake_result
    }

    pub fn incoming(&mut self) -> &mut mpsc::UnboundedReceiver<TransportMessage> {
        &mut self.incoming
    }
}

fn parse_initial_messages(
    protocol: &Arc<dyn HubProtocol>,
    handshake_result: &HandshakeResult,
) -> Result<Vec<HubMessage>, Error> {
    let Some(remaining) = &handshake_result.remaining else {
        return Ok(Vec::new());
    };

    let input = match remaining {
        RemainingData::Text(text) => HubProtocolInput::Text(text),
        RemainingData::Bytes(bytes) => HubProtocolInput::Binary(bytes),
    };

    protocol.parse_messages(input).map_err(Error::from)
}

async fn resolve_access_token(
    factory: Option<&AccessTokenFactory>,
) -> Result<Option<String>, Error> {
    let Some(factory) = factory else {
        return Ok(None);
    };

    let token = factory().await?;
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

fn parse_transport_handshake(message: TransportMessage) -> Result<HandshakeResult, Error> {
    match message {
        TransportMessage::Text(text) => {
            HandshakeProtocol.parse_response(HandshakeInput::Text(&text))
        }
        TransportMessage::Binary(bytes) => {
            HandshakeProtocol.parse_response(HandshakeInput::Binary(&bytes))
        }
        TransportMessage::Closed(reason) => Err(Error::Handshake(format!(
            "connection closed during handshake: {}",
            reason.unwrap_or_else(|| "unknown reason".into())
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::RECORD_SEPARATOR;

    #[test]
    fn parse_transport_handshake_accepts_text_response() {
        let payload = format!(r#"{{"error":null}}{RECORD_SEPARATOR}"#);
        let result = parse_transport_handshake(TransportMessage::Text(payload)).unwrap();
        assert!(result.response.error.is_none());
    }
}
