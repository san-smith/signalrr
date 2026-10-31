//! In-process SignalR hub for tests of code built on this client (feature `test-support`).
//!
//! One TCP port serves both negotiate (a minimal HTTP/1.1 responder) and the WebSocket
//! (`accept_hdr_async`). The test decides what negotiate answers, takes every accepted
//! WebSocket as a [`ServerConnection`] and drives it: handshake, frames to send, frames the
//! client sent, close or abort.
//!
//! ```no_run
//! # async fn example() -> Result<(), signalrr::Error> {
//! use signalrr::HubConnection;
//! use signalrr::test_support::FakeHub;
//!
//! let hub = FakeHub::start().await;
//! let connection = HubConnection::builder(&hub.url).with_json_protocol().build();
//! let (started, mut server) = tokio::join!(connection.start(), async {
//!     let mut server = hub.accept().await;
//!     server.handshake().await;
//!     server
//! });
//! started?;
//! let message = server.recv().await; // what the client sent
//! # let _ = message;
//! # Ok(())
//! # }
//! ```
//!
//! This is test scaffolding: on anything unexpected (the client stays silent past
//! [`TIMEOUT`], sends a malformed frame, closes early) it panics with a message, which fails
//! the test that drives it.

// Panicking is how the fake hub fails a test. `result_large_err`: the `accept_hdr_async`
// callback signature comes from tungstenite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
#[cfg(feature = "messagepack-brotli")]
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

#[cfg(feature = "messagepack-brotli")]
use crate::BinaryMessageFormat;
use crate::{HubMessage, HubProtocol, HubProtocolInput, JsonProtocol, RECORD_SEPARATOR};

/// How long a test waits for the client before failing.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Answer to one negotiate request.
pub enum NegotiateReply {
    /// `200` with the default body for the request: a fresh connection token and
    /// `useStatefulReconnect` when the client asked for it.
    Ok,
    /// `200` with this body.
    Json(Value),
    /// An empty response with this status.
    Status(u16),
}

/// A negotiate request as the server saw it.
#[derive(Debug, Clone)]
pub struct NegotiateRequest {
    pub path_and_query: String,
    pub authorization: Option<String>,
    pub headers: HashMap<String, String>,
}

pub struct FakeHub {
    /// Hub URL for the client, `http://127.0.0.1:{port}/hub`.
    pub url: String,
    /// Base address, `http://127.0.0.1:{port}`, for redirects to other paths.
    pub origin: String,
    replies: Arc<Mutex<VecDeque<NegotiateReply>>>,
    requests: Arc<Mutex<Vec<NegotiateRequest>>>,
    connections: tokio::sync::Mutex<mpsc::UnboundedReceiver<ServerConnection>>,
    refuse_websockets: Arc<AtomicBool>,
    refused_tokens: Arc<Mutex<Vec<String>>>,
}

impl FakeHub {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let replies = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (connections_tx, connections) = mpsc::unbounded_channel();
        let refuse_websockets = Arc::new(AtomicBool::new(false));
        let refused_tokens: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        tokio::spawn({
            let replies = replies.clone();
            let requests = requests.clone();
            let refuse_websockets = refuse_websockets.clone();
            let refused_tokens = refused_tokens.clone();
            async move {
                let mut tokens = 0;
                while let Ok((stream, _)) = listener.accept().await {
                    let mut method = [0; 4];
                    if stream.peek(&mut method).await.is_err() {
                        continue;
                    }
                    if &method == b"POST" {
                        tokens += 1;
                        let reply = replies.lock().unwrap().pop_front();
                        let token = format!("token-{tokens}");
                        tokio::spawn(negotiate(stream, reply, token, requests.clone()));
                    } else if refuse_websockets.load(Ordering::SeqCst) {
                        drop(stream);
                    } else {
                        let connections_tx = connections_tx.clone();
                        let refused_tokens = refused_tokens.clone();
                        tokio::spawn(async move {
                            let refused = refused_tokens.lock().unwrap().clone();
                            if let Some(connection) =
                                ServerConnection::accept(stream, &refused).await
                            {
                                let _ = connections_tx.send(connection);
                            }
                        });
                    }
                }
            }
        });

        Self {
            url: format!("{origin}/hub"),
            origin,
            replies,
            requests,
            connections: tokio::sync::Mutex::new(connections),
            refuse_websockets,
            refused_tokens,
        }
    }

    /// Queues the answer to the next negotiate request; without one the reply is
    /// [`NegotiateReply::Ok`].
    pub fn reply_to_negotiate(&self, reply: NegotiateReply) {
        self.replies.lock().unwrap().push_back(reply);
    }

    /// Rejects the WebSocket upgrade for this connection token, as a server does for a
    /// connection it no longer knows.
    pub fn refuse_connection_token(&self, token: &str) {
        self.refused_tokens.lock().unwrap().push(token.to_string());
    }

    /// Drops every further WebSocket connection before the upgrade.
    pub fn refuse_websockets(&self) {
        self.refuse_websockets.store(true, Ordering::SeqCst);
    }

    pub fn negotiate_requests(&self) -> Vec<NegotiateRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Waits for the next WebSocket connection.
    pub async fn accept(&self) -> ServerConnection {
        let mut connections = self.connections.lock().await;
        tokio::time::timeout(TIMEOUT, connections.recv())
            .await
            .expect("client did not open a WebSocket")
            .expect("fake hub stopped")
    }
}

async fn negotiate(
    mut stream: TcpStream,
    reply: Option<NegotiateReply>,
    token: String,
    requests: Arc<Mutex<Vec<NegotiateRequest>>>,
) {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0; 1];
        if stream.read_exact(&mut byte).await.is_err() {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let path_and_query = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let body_len = headers
        .get("content-length")
        .and_then(|len| len.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; body_len];
    let _ = stream.read_exact(&mut body).await;

    let stateful = path_and_query.contains("useStatefulReconnect=true");
    requests.lock().unwrap().push(NegotiateRequest {
        authorization: headers.get("authorization").cloned(),
        path_and_query,
        headers,
    });

    let (status, body) = match reply.unwrap_or(NegotiateReply::Ok) {
        NegotiateReply::Ok => {
            let mut body = json!({
                "negotiateVersion": 1,
                "connectionId": format!("id-{token}"),
                "connectionToken": token,
                "availableTransports": [
                    { "transport": "WebSockets", "transferFormats": ["Text", "Binary"] }
                ],
            });
            if stateful {
                body["useStatefulReconnect"] = json!(true);
            }
            (200, body.to_string())
        }
        NegotiateReply::Json(body) => (200, body.to_string()),
        NegotiateReply::Status(status) => (status, String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The server side of one WebSocket connection.
pub struct ServerConnection {
    /// Path of the WebSocket request.
    pub path: String,
    /// Query parameters of the WebSocket request.
    pub query: HashMap<String, Vec<String>>,
    ws: WebSocketStream<TcpStream>,
    protocol: Option<Arc<dyn HubProtocol>>,
    pending: VecDeque<HubMessage>,
}

impl ServerConnection {
    async fn accept(stream: TcpStream, refused_tokens: &[String]) -> Option<Self> {
        let mut uri = None;
        let ws = tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response| {
            let query = request.uri().query().unwrap_or("");
            let refused = url::form_urlencoded::parse(query.as_bytes())
                .any(|(key, value)| key == "id" && refused_tokens.iter().any(|t| *t == value));
            if refused {
                let mut not_found = tokio_tungstenite::tungstenite::http::Response::new(None);
                *not_found.status_mut() =
                    tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
                return Err(not_found);
            }
            uri = Some(request.uri().clone());
            Ok::<Response, _>(response)
        })
        .await
        .ok()?;
        let uri = uri?;
        let mut query: HashMap<String, Vec<String>> = HashMap::new();
        for (key, value) in url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
            query
                .entry(key.into_owned())
                .or_default()
                .push(value.into_owned());
        }
        Some(Self {
            path: uri.path().to_string(),
            query,
            ws,
            protocol: None,
            pending: VecDeque::new(),
        })
    }

    /// The single value of a query parameter.
    pub fn query_value(&self, key: &str) -> Option<&str> {
        match self.query.get(key).map(Vec::as_slice) {
            Some([value]) => Some(value),
            Some(values) => panic!("{key} appears {} times in the query", values.len()),
            None => None,
        }
    }

    /// Reads the handshake request: `{"protocol": ..., "version": ...}`.
    pub async fn handshake_request(&mut self) -> Value {
        let text = match self.next_frame().await {
            Some(Message::Text(text)) => text,
            other => panic!("expected a text handshake, got {other:?}"),
        };
        let request = text
            .strip_suffix(RECORD_SEPARATOR)
            .expect("handshake ends with the record separator");
        let request: Value = serde_json::from_str(request).unwrap();
        self.assume_protocol(request["protocol"].as_str().unwrap());
        request
    }

    /// Sets the hub protocol without a handshake, for a transport that resumes an existing
    /// connection (stateful reconnect).
    pub fn assume_protocol(&mut self, name: &str) {
        self.protocol = Some(match name {
            "json" => Arc::new(JsonProtocol),
            #[cfg(feature = "messagepack")]
            "messagepack" => Arc::new(crate::MessagePackProtocol),
            #[cfg(feature = "messagepack-brotli")]
            "messagepack+brotli" => Arc::new(crate::MessagePackBrotliProtocol::default()),
            other => panic!("unknown protocol {other}"),
        });
    }

    /// Answers the handshake: `{}` or `{"error": ...}`.
    pub async fn handshake_response(&mut self, error: Option<&str>) {
        let body = match error {
            Some(error) => json!({ "error": error }).to_string(),
            None => "{}".to_string(),
        };
        self.send_frame(Message::Text(format!("{body}{RECORD_SEPARATOR}").into()))
            .await;
    }

    /// Reads the handshake request, accepts it and returns the request.
    pub async fn handshake(&mut self) -> Value {
        let request = self.handshake_request().await;
        self.handshake_response(None).await;
        request
    }

    /// The next message from the client, Ping included.
    pub async fn recv_any(&mut self) -> HubMessage {
        loop {
            if let Some(message) = self.pending.pop_front() {
                return message;
            }
            let protocol = self.protocol.clone().expect("handshake first");
            match self.next_frame().await {
                Some(Message::Text(text)) => self.pending.extend(
                    protocol
                        .parse_messages(HubProtocolInput::Text(&text))
                        .unwrap(),
                ),
                Some(Message::Binary(bytes)) => self.pending.extend(
                    protocol
                        .parse_messages(HubProtocolInput::Binary(&bytes))
                        .unwrap(),
                ),
                Some(Message::Close(_)) | None => panic!("client closed the connection"),
                Some(_) => {}
            }
        }
    }

    /// The next message from the client other than Ping.
    pub async fn recv(&mut self) -> HubMessage {
        loop {
            let message = self.recv_any().await;
            if !matches!(message, HubMessage::Ping(_)) {
                return message;
            }
        }
    }

    /// Waits until the client closes the WebSocket; panics on any message but Ping and
    /// Close.
    pub async fn expect_closed(&mut self) {
        loop {
            match self.next_frame().await {
                Some(Message::Close(_)) | None => return,
                Some(Message::Ping(_) | Message::Pong(_)) => {}
                Some(Message::Text(text)) => {
                    let messages = self
                        .protocol
                        .as_ref()
                        .expect("handshake first")
                        .parse_messages(HubProtocolInput::Text(&text))
                        .unwrap();
                    assert!(
                        messages.iter().all(is_ping_or_close),
                        "unexpected {messages:?}"
                    );
                }
                Some(Message::Binary(bytes)) => {
                    let messages = self
                        .protocol
                        .as_ref()
                        .expect("handshake first")
                        .parse_messages(HubProtocolInput::Binary(&bytes))
                        .unwrap();
                    assert!(
                        messages.iter().all(is_ping_or_close),
                        "unexpected {messages:?}"
                    );
                }
                Some(Message::Frame(_)) => {}
            }
        }
    }

    /// Sends a message encoded with the negotiated protocol.
    pub async fn send(&mut self, message: &HubMessage) {
        let protocol = self.protocol.clone().expect("handshake first");
        let bytes = protocol.write_message(message).unwrap();
        let frame = match protocol.transfer_format() {
            crate::TransferFormat::Text => Message::Text(String::from_utf8(bytes).unwrap().into()),
            crate::TransferFormat::Binary => Message::Binary(bytes.into()),
        };
        self.send_frame(frame).await;
    }

    /// Sends a prepared binary frame.
    pub async fn send_binary(&mut self, frame: Vec<u8>) {
        self.send_frame(Message::Binary(frame.into())).await;
    }

    /// Closes the WebSocket with a close frame.
    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }

    /// Drops the TCP connection without a close frame.
    pub fn abort(self) {
        drop(self);
    }

    async fn send_frame(&mut self, frame: Message) {
        self.ws.send(frame).await.unwrap();
    }

    async fn next_frame(&mut self) -> Option<Message> {
        tokio::time::timeout(TIMEOUT, self.ws.next())
            .await
            .expect("client sent nothing")
            .and_then(Result::ok)
    }
}

fn is_ping_or_close(message: &HubMessage) -> bool {
    matches!(message, HubMessage::Ping(_) | HubMessage::Close(_))
}

/// A `messagepack+brotli` invocation frame with the arguments brotli-compressed, the way
/// the server sends large payloads.
#[cfg(feature = "messagepack-brotli")]
pub fn compressed_invocation(target: &str, arguments: rmpv::Value) -> Vec<u8> {
    use rmpv::Value as MsgValue;

    let encode = |value: &MsgValue| {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value).unwrap();
        bytes
    };
    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 5, 22);
        writer.write_all(&encode(&arguments)).unwrap();
    }
    let message = MsgValue::Array(vec![
        MsgValue::from(1),
        MsgValue::Map(vec![]),
        MsgValue::Nil,
        MsgValue::from(target),
        MsgValue::from(1),
        MsgValue::Binary(compressed),
    ]);
    BinaryMessageFormat::write(&encode(&message))
}
