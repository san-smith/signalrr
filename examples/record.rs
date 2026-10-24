//! Records every frame a SignalR hub sends: the raw frame, decompressed `messagepack+brotli`
//! payloads, and the parsed messages as JSON.
//!
//! The example drives the transport directly instead of `HubConnection`, so it sees each frame
//! before parsing. Configuration comes from the environment:
//!
//! - `SIGNALR_URL` (required): hub URL, e.g. `https://host/hub`.
//! - `SIGNALR_PROTOCOL`: `json` (default), `messagepack` or `messagepack+brotli`.
//! - `SIGNALR_STATEFUL_RECONNECT=1`: request stateful reconnect and acknowledge messages.
//! - `ACCESS_TOKEN`: bearer token for negotiate and the WebSocket.
//! - `SIGNALR_HEADERS`: extra negotiate headers, one `Name: value` per line.
//! - `SIGNALR_CALLS`: path to a JSON file with invocations sent after the handshake:
//!   `[{"method": "Subscribe", "args": [...], "invocationId": "1"}]`. Without
//!   `invocationId` the call is fire-and-forget and the server sends no completion.
//! - `SIGNALR_OUT`: output directory, `frames` by default.
//! - `SIGNALR_DURATION_SECS`: how long to record, 30 by default.
//! - `SIGNALR_MAX_FRAMES`: stop after this many frames.
//!
//! For incoming frame `N` (from 1, the handshake response included) it writes:
//!
//! - `N.raw.txt` or `N.raw.bin`: the frame as received;
//! - `N.handshake.json`: the handshake response, for the first frame;
//! - `N.messages.json`: parsed messages, or `N.error.txt` when parsing fails;
//! - `N.M.payload.msgpack`: decompressed payload of message `M` (from 0) in the frame, for
//!   `messagepack+brotli` messages with a compressed payload.
//!
//! Run with:
//! `SIGNALR_URL=https://host/hub cargo run --example record --features messagepack-brotli`

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use signalrr::{
    AckMessage, BinaryMessageFormat, BrotliDecoder, DefaultBrotliDecoder, HandshakeInput,
    HandshakeProtocol, HandshakeRequest, HubMessage, HubProtocol, HubProtocolInput,
    InvocationMessage, JsonProtocol, MessagePackBrotliProtocol, MessagePackProtocol,
    NegotiateOptions, PingMessage, RemainingData, TransferFormat, TransportMessage,
    WebSocketConnectOptions, WebSocketTransport, build_websocket_url, negotiate_with_client,
    resolve_handshake_version,
};

type BoxError = Box<dyn Error>;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Call {
    method: String,
    #[serde(default)]
    args: Vec<serde_json::Value>,
    invocation_id: Option<String>,
}

struct Recorder {
    out: PathBuf,
    protocol: Arc<dyn HubProtocol>,
    brotli: bool,
    frames: usize,
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let url = std::env::var("SIGNALR_URL").map_err(|_| "SIGNALR_URL must be set")?;
    let protocol_name = env_or("SIGNALR_PROTOCOL", "json");
    let protocol: Arc<dyn HubProtocol> = match protocol_name.as_str() {
        "json" => Arc::new(JsonProtocol),
        "messagepack" => Arc::new(MessagePackProtocol),
        "messagepack+brotli" => Arc::new(MessagePackBrotliProtocol::default()),
        other => return Err(format!("unsupported SIGNALR_PROTOCOL `{other}`").into()),
    };
    let stateful = std::env::var("SIGNALR_STATEFUL_RECONNECT").is_ok_and(|value| value == "1");
    let token = std::env::var("ACCESS_TOKEN").ok();
    let calls: Vec<Call> = match std::env::var("SIGNALR_CALLS") {
        Ok(path) => serde_json::from_str(&std::fs::read_to_string(path)?)?,
        Err(_) => Vec::new(),
    };
    let duration = Duration::from_secs(env_or("SIGNALR_DURATION_SECS", "30").parse()?);
    let max_frames = match std::env::var("SIGNALR_MAX_FRAMES") {
        Ok(value) => Some(value.parse::<usize>()?),
        Err(_) => None,
    };

    let mut options = NegotiateOptions::new();
    options.use_stateful_reconnect = stateful;
    for line in env_or("SIGNALR_HEADERS", "").lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("header `{line}` is not `Name: value`"))?;
        options
            .headers
            .push((name.trim().to_string(), value.trim().to_string()));
    }

    let client = reqwest::Client::builder()
        .timeout(options.timeout)
        .build()?;
    let negotiated = negotiate_with_client(&url, &options, &client, token.as_deref()).await?;
    let ack_messages = stateful && negotiated.response.use_stateful_reconnect == Some(true);
    let websocket_url = build_websocket_url(
        &url,
        negotiated.response.websocket_token()?,
        negotiated.access_token.as_deref(),
    )?;
    // The URL already carries the token, so the transport gets none.
    let (transport, mut incoming) = WebSocketTransport::connect_with_options(
        &websocket_url,
        None,
        WebSocketConnectOptions::default(),
    )
    .await?;

    let handshake = HandshakeRequest {
        protocol: protocol.name().to_string(),
        version: resolve_handshake_version(stateful, protocol.version(), None),
    };
    transport
        .send_text(HandshakeProtocol.write_request(&handshake)?)
        .await?;

    let out = PathBuf::from(env_or("SIGNALR_OUT", "frames"));
    std::fs::create_dir_all(&out)?;
    let mut recorder = Recorder {
        out,
        brotli: protocol_name == "messagepack+brotli",
        protocol: protocol.clone(),
        frames: 0,
    };
    eprintln!(
        "recording {} with {} into {}",
        url,
        protocol.name(),
        recorder.out.display()
    );

    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    keep_alive.tick().await;
    let mut handshake_done = false;
    let mut received = 0_i64;
    let mut acked = 0_i64;

    loop {
        let message = tokio::select! {
            () = &mut deadline => break,
            _ = keep_alive.tick() => {
                send(&transport, protocol.as_ref(), &HubMessage::Ping(PingMessage)).await?;
                continue;
            }
            message = incoming.recv() => message,
        };
        let messages = match message {
            Some(TransportMessage::Closed(reason)) => {
                eprintln!("connection closed: {}", reason.unwrap_or_default());
                break;
            }
            None => break,
            Some(frame) if !handshake_done => {
                handshake_done = true;
                let messages = recorder.handshake(&frame)?;
                for call in &calls {
                    let invocation = HubMessage::Invocation(InvocationMessage {
                        invocation_id: call.invocation_id.clone(),
                        target: call.method.clone(),
                        arguments: call.args.clone(),
                        ..Default::default()
                    });
                    send(&transport, protocol.as_ref(), &invocation).await?;
                }
                messages
            }
            Some(frame) => recorder.frame(&frame)?,
        };

        received += messages
            .iter()
            .filter(|message| is_sequenced(message))
            .count() as i64;
        if ack_messages && received > acked {
            acked = received;
            let ack = HubMessage::Ack(AckMessage { sequence_id: acked });
            send(&transport, protocol.as_ref(), &ack).await?;
        }
        if max_frames.is_some_and(|max| recorder.frames >= max) {
            break;
        }
    }

    eprintln!("recorded {} frames", recorder.frames);
    transport.close().await?;
    Ok(())
}

impl Recorder {
    /// Records the handshake response and returns the messages that followed it.
    fn handshake(&mut self, frame: &TransportMessage) -> Result<Vec<HubMessage>, BoxError> {
        let base = self.write_raw(frame)?;
        let result = match frame {
            TransportMessage::Text(text) => {
                HandshakeProtocol.parse_response(HandshakeInput::Text(text))?
            }
            TransportMessage::Binary(bytes) => {
                HandshakeProtocol.parse_response(HandshakeInput::Binary(bytes))?
            }
            TransportMessage::Closed(_) => return Ok(Vec::new()),
        };
        let response = serde_json::json!({
            "error": result.response.error,
            "minorVersion": result.response.minor_version,
        });
        std::fs::write(
            with_suffix(&base, "handshake.json"),
            serde_json::to_vec_pretty(&response)?,
        )?;
        if let Some(error) = result.response.error {
            return Err(format!("handshake rejected: {error}").into());
        }

        match result.remaining {
            Some(RemainingData::Text(text)) => self.parse(&base, HubProtocolInput::Text(&text)),
            Some(RemainingData::Bytes(bytes)) => {
                self.parse(&base, HubProtocolInput::Binary(&bytes))
            }
            None => Ok(Vec::new()),
        }
    }

    /// Records a frame after the handshake and returns its parsed messages.
    fn frame(&mut self, frame: &TransportMessage) -> Result<Vec<HubMessage>, BoxError> {
        let base = self.write_raw(frame)?;
        match frame {
            TransportMessage::Text(text) => self.parse(&base, HubProtocolInput::Text(text)),
            TransportMessage::Binary(bytes) => self.parse(&base, HubProtocolInput::Binary(bytes)),
            TransportMessage::Closed(_) => Ok(Vec::new()),
        }
    }

    /// Writes the raw frame and returns the path prefix for its other files.
    fn write_raw(&mut self, frame: &TransportMessage) -> Result<PathBuf, BoxError> {
        self.frames += 1;
        let base = self.out.join(format!("{:06}", self.frames));
        match frame {
            TransportMessage::Text(text) => std::fs::write(with_suffix(&base, "raw.txt"), text)?,
            TransportMessage::Binary(bytes) => {
                std::fs::write(with_suffix(&base, "raw.bin"), bytes)?
            }
            TransportMessage::Closed(_) => {}
        }
        Ok(base)
    }

    /// Writes the parsed messages and decompressed payloads; a parse error is recorded, not
    /// returned, so one odd frame does not stop the recording.
    fn parse(&self, base: &Path, input: HubProtocolInput<'_>) -> Result<Vec<HubMessage>, BoxError> {
        if let (true, HubProtocolInput::Binary(bytes)) = (self.brotli, input) {
            for (index, payload) in compressed_payloads(bytes)? {
                std::fs::write(
                    with_suffix(base, &format!("{index}.payload.msgpack")),
                    payload,
                )?;
            }
        }

        match self.protocol.parse_messages(input) {
            Ok(messages) => {
                let json: Vec<_> = messages.iter().map(HubMessage::to_json_value).collect();
                std::fs::write(
                    with_suffix(base, "messages.json"),
                    serde_json::to_vec_pretty(&json)?,
                )?;
                Ok(messages)
            }
            Err(error) => {
                std::fs::write(with_suffix(base, "error.txt"), error.to_string())?;
                Ok(Vec::new())
            }
        }
    }
}

/// Decompresses the payloads of `messagepack+brotli` messages in a frame, keyed by the index of
/// the message in the frame.
///
/// Layouts (compression flag, then payload): Invocation and StreamInvocation
/// `[type, headers, invocationId, target, isCompressed, arguments, ...]`, StreamItem
/// `[2, headers, invocationId, isCompressed, item]`, Completion
/// `[3, headers, invocationId, resultKind, isCompressed, result]`.
fn compressed_payloads(frame: &[u8]) -> Result<Vec<(usize, Vec<u8>)>, BoxError> {
    let mut payloads = Vec::new();
    for (index, message) in BinaryMessageFormat::parse(frame)?.into_iter().enumerate() {
        let rmpv::Value::Array(items) = rmpv::decode::read_value(&mut message.as_slice())? else {
            continue;
        };
        let (flag, payload) = match items.first().and_then(rmpv::Value::as_i64) {
            Some(1 | 3 | 4) => (4, 5),
            Some(2) => (3, 4),
            _ => continue,
        };
        if items.get(flag).and_then(rmpv::Value::as_i64) != Some(1) {
            continue;
        }
        if let Some(rmpv::Value::Binary(data)) = items.get(payload) {
            payloads.push((index, DefaultBrotliDecoder.decode(data)?));
        }
    }
    Ok(payloads)
}

async fn send(
    transport: &WebSocketTransport,
    protocol: &dyn HubProtocol,
    message: &HubMessage,
) -> Result<(), BoxError> {
    let bytes = protocol.write_message(message)?;
    match protocol.transfer_format() {
        TransferFormat::Text => transport.send_text(String::from_utf8(bytes)?).await?,
        TransferFormat::Binary => transport.send_binary(bytes).await?,
    }
    Ok(())
}

/// Messages that count towards the stateful reconnect sequence.
fn is_sequenced(message: &HubMessage) -> bool {
    matches!(
        message,
        HubMessage::Invocation(_)
            | HubMessage::StreamInvocation(_)
            | HubMessage::StreamItem(_)
            | HubMessage::Completion(_)
            | HubMessage::CancelInvocation(_)
    )
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(".");
    path.push(suffix);
    PathBuf::from(path)
}
