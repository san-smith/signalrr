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
use crate::negotiate::{
    NegotiateOptions, NegotiateResult, build_websocket_url, negotiate_with_token_factory,
};
use crate::transport::{
    TransferFormat, TransportMessage, WebSocketConnectOptions, WebSocketTransport,
};
use reqwest::Client;

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
    pub http_client: Option<Client>,
    pub log_message_content: bool,
}

impl ConnectionOptions {
    pub fn new(base_url: impl Into<String>) -> Self {
        let hub_protocol = crate::hub::default_hub_protocol();
        Self {
            base_url: base_url.into(),
            negotiate: NegotiateOptions::new(),
            handshake_request: HandshakeRequest {
                protocol: hub_protocol.name().into(),
                version: hub_protocol.version(),
            },
            handshake_version_override: None,
            transfer_format: hub_protocol.transfer_format(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            access_token_factory: None,
            hub_protocol,
            server_timeout: DEFAULT_SERVER_TIMEOUT,
            keep_alive_interval: DEFAULT_KEEP_ALIVE_INTERVAL,
            reconnect_policy: None,
            stateful_reconnect_buffer_size: DEFAULT_STATEFUL_RECONNECT_BUFFER_SIZE,
            http_client: None,
            log_message_content: false,
        }
    }
}

/// What a stateful reconnect needs to reopen the transport of an established connection.
#[derive(Clone)]
pub(crate) struct ResumeTarget {
    /// WebSocket URL with the connection token, without the access token.
    url: String,
    /// Token the server issued during negotiate; reused as is. Otherwise the token comes
    /// from the access token factory again.
    server_token: Option<String>,
}

impl ResumeTarget {
    /// Reopens the WebSocket of the connection, without negotiate and handshake.
    pub(crate) async fn reopen(
        &self,
        options: &ConnectionOptions,
    ) -> Result<
        (
            WebSocketTransport,
            mpsc::UnboundedReceiver<TransportMessage>,
        ),
        Error,
    > {
        let token = match &self.server_token {
            Some(token) => Some(token.clone()),
            None => crate::access_token::resolve(options.access_token_factory.as_ref()).await?,
        };
        let url = crate::transport::append_access_token(&self.url, token.as_deref())?;
        debug!(url = %self.url, "reopening websocket transport");
        let (mut transport, incoming) = WebSocketTransport::connect_with_options(
            &url,
            None,
            WebSocketConnectOptions {
                log_message_content: options.log_message_content,
            },
        )
        .await?;
        transport.enable_send_queue();
        Ok((transport, incoming))
    }
}

/// Established SignalR transport connection.
pub struct Connection {
    base_url: String,
    connection_id: Option<String>,
    connection_token: String,
    transport: WebSocketTransport,
    handshake_result: HandshakeResult,
    initial_messages: Vec<HubMessage>,
    incoming: mpsc::UnboundedReceiver<TransportMessage>,
    /// Present when the server agreed to stateful reconnect.
    resume: Option<ResumeTarget>,
}

impl Connection {
    /// Negotiates, opens the WebSocket, and completes the SignalR handshake.
    pub async fn connect(options: ConnectionOptions) -> Result<Self, Error> {
        let NegotiateResult {
            response,
            access_token,
            url,
            token_from_server,
        } = {
            let client = match options.http_client.clone() {
                Some(client) => client,
                None => Client::builder()
                    .timeout(options.negotiate.timeout)
                    .build()?,
            };
            negotiate_with_token_factory(
                &options.base_url,
                &options.negotiate,
                &client,
                options.access_token_factory.as_ref(),
            )
            .await?
        };

        let connection_token = response.websocket_token()?.to_string();
        let connection_id = response.connection_id.clone();
        let websocket_url = build_websocket_url(&url, &connection_token, access_token.as_deref())?;
        let resume = match response.use_stateful_reconnect {
            Some(true) => Some(ResumeTarget {
                url: build_websocket_url(&url, &connection_token, None)?,
                server_token: if token_from_server {
                    access_token.clone()
                } else {
                    None
                },
            }),
            _ => None,
        };

        debug!(url = %websocket_url, "starting websocket transport");
        // `build_websocket_url` has already put the token into the URL.
        let (mut transport, mut incoming) = WebSocketTransport::connect_with_options(
            &websocket_url,
            None,
            WebSocketConnectOptions {
                log_message_content: options.log_message_content,
            },
        )
        .await?;
        transport.enable_send_queue();

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
            resume,
        })
    }

    /// How to reopen the transport; `None` when stateful reconnect was not negotiated.
    pub(crate) fn resume_target(&self) -> Option<ResumeTarget> {
        self.resume.clone()
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

    // The `accept_hdr_async` callback signature comes from tungstenite.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn connect_puts_access_token_into_websocket_url_once() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Negotiate: a POST without a body, answered with a minimal HTTP response.
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0; 1];
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let body = r#"{"connectionId":"id","connectionToken":"token","negotiateVersion":1}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            drop(stream);

            let (stream, _) = listener.accept().await.unwrap();
            let mut query = String::new();
            let mut websocket =
                tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response| {
                    query = request.uri().query().unwrap_or_default().to_string();
                    Ok::<Response, _>(response)
                })
                .await
                .unwrap();
            websocket.next().await.unwrap().unwrap();
            let handshake = format!("{{}}{RECORD_SEPARATOR}");
            websocket.send(Message::Text(handshake)).await.unwrap();
            query
        });

        let mut options = ConnectionOptions::new(format!("http://{address}/hub"));
        options.access_token_factory = Some(crate::access_token::factory(|| async {
            Ok("secret".to_string())
        }));
        let connection = Connection::connect(options).await.unwrap();

        let query = server.await.unwrap();
        assert_eq!(query.matches("access_token=secret").count(), 1, "{query}");
        drop(connection);
    }
}
