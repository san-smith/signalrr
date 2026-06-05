//! Internal SignalR connection: negotiate, WebSocket, and handshake.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time;
use tracing::{debug, info};

use crate::access_token::AccessTokenFactory;
use crate::error::Error;
use crate::handshake::{HandshakeInput, HandshakeProtocol, HandshakeRequest, HandshakeResult, RemainingData};
use crate::negotiate::{NegotiateOptions, NegotiateResult, build_websocket_url, negotiate};
use crate::transport::{TransferFormat, WebSocketTransport, TransportMessage};

const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Configuration for establishing a transport connection.
#[derive(Clone)]
pub struct ConnectionOptions {
    pub base_url: String,
    pub negotiate: NegotiateOptions,
    pub handshake_request: HandshakeRequest,
    pub transfer_format: TransferFormat,
    pub handshake_timeout: Duration,
    pub access_token_factory: Option<AccessTokenFactory>,
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
            transfer_format: TransferFormat::Binary,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            access_token_factory: None,
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

        let handshake_payload = HandshakeProtocol.write_request(&options.handshake_request)?;
        transport.send_text(handshake_payload).await?;

        let handshake_message = time::timeout(options.handshake_timeout, incoming.recv())
            .await
            .map_err(|_| Error::Handshake("handshake timeout".into()))?
            .ok_or_else(|| Error::Handshake("connection closed during handshake".into()))?;

        let handshake_result = parse_transport_handshake(handshake_message)?;
        if let Some(error) = &handshake_result.response.error {
            return Err(Error::Handshake(error.clone()));
        }

        info!(
            connection_id = connection_id.as_deref().unwrap_or("unknown"),
            protocol = %options.handshake_request.protocol,
            "signalr handshake completed"
        );

        Ok(Self {
            base_url: options.base_url,
            connection_id,
            connection_token,
            transport,
            handshake_result,
            incoming,
        })
    }

    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    pub fn remaining_handshake_data(&self) -> Option<&RemainingData> {
        self.handshake_result.remaining.as_ref()
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
