# signalrr

[![docs.rs](https://img.shields.io/docsrs/signalrr)](https://docs.rs/signalrr)

Rust client for **ASP.NET Core SignalR** with support for MessagePack and custom hub protocols.

## Status

> Work in progress — API is not stable.

Implemented:

- text/binary message framing
- JSON handshake
- HTTP negotiate with redirects and `connectionToken`; on `401` a fresh token is requested
  from the access token factory and the request is retried once (`Error::Unauthorized`
  otherwise)
- WebSocket transport
- `HubConnection::start()` / `close()` lifecycle
- hub message model (`HubMessage`, invocation/completion/ping/…)
- JSON and MessagePack v2 hub protocols (`HubProtocol`)
- builder helpers: `with_json_protocol()`, `with_messagepack_protocol()`
- `invoke` / `send` / `on` hub methods
- server timeout (30s) and keepalive ping (15s)
- custom `messagepack+brotli` hub protocol (feature `messagepack-brotli`)
- automatic reconnect with configurable [`RetryPolicy`](https://docs.rs/signalrr/latest/signalrr/trait.RetryPolicy.html)
- stateful reconnect (`with_stateful_reconnect()`): after a transport drop the same
  connection resumes over a new WebSocket and unacknowledged messages are resent, without
  a new negotiate and without reconnect callbacks
- streaming hub invocations (`stream()` / `StreamInvocation`)
- automatic handshake version selection (legacy v1 without stateful reconnect, hub protocol v2 with stateful reconnect)
- lifecycle callbacks: `on_close`, `on_reconnecting`, `on_reconnected`
- pending invocation cancellation on disconnect and server `CloseMessage` handling
- multiple client handlers per hub method (`on` / `on_async`, `off` / `off_handler`)
- typed handlers (`on_typed`): arguments are deserialized into your type straight from the
  received MessagePack bytes, without an intermediate `serde_json::Value`
- client-to-server streaming hub method arguments (`HubArgument::Stream`)
- transport send queue for ordered WebSocket writes
- injectable HTTP client for negotiate (`with_http_client`)
- optional message content logging (`with_log_message_content`)
- mutable `set_base_url` while disconnected
- handshake `initial_messages` / `remaining_handshake_data` exposed on `HubConnection`
- start/stop race handling when `close()` is called during `start()`

Planned next:

- FFI/WASM bindings (deferred)

## Example

```rust
use futures_util::StreamExt;
use signalrr::{ClientUploadStream, HubArgument, HubConnection};

#[tokio::main]
async fn main() -> Result<(), signalrr::Error> {
    let mut connection = HubConnection::builder("https://example.com/signalr?platform=pwa")
        .with_messagepack_protocol()
        .with_automatic_reconnect()
        .with_access_token(|| async {
            Ok(std::env::var("ACCESS_TOKEN").unwrap_or_default())
        })
        .with_header("X-Tenant-Id", "brand.region")
        .build();

    connection.start().await?;
    println!("Connected: {:?}", connection.connection_id().await);

    connection
        .on("ReceiveMessage", |args| {
            println!("Message: {args:?}");
            None
        })
        .await;

    #[derive(serde::Deserialize, Debug)]
    struct Price {
        id: i64,
        value: f64,
    }
    // A tuple with one element per argument of the server call.
    connection
        .on_typed("PriceChanged", |(price,): (Price,)| println!("Price: {price:?}"))
        .await;

    let upload = ClientUploadStream::from_iter([serde_json::json!("chunk")]);
    connection
        .send(
            "Upload",
            vec![
                HubArgument::Value(serde_json::json!("metadata")),
                HubArgument::Stream(upload),
            ],
        )
        .await?;

    let mut stream = connection.stream("WatchCounter", vec![]).await?;
    while let Some(item) = stream.next().await {
        println!("Stream item: {:?}", item?);
    }

    connection.close().await?;
    Ok(())
}
```

## Recording frames

`examples/record.rs` connects to any hub, sends the invocations listed in a JSON file and
writes every incoming frame to disk: the raw frame, the parsed messages as JSON and, for
`messagepack+brotli`, the decompressed payloads. Use it to inspect a server or to collect
test fixtures.

```bash
echo '[{"method": "Subscribe", "args": ["prices"]}]' > calls.json
SIGNALR_URL=https://example.com/hub SIGNALR_PROTOCOL=messagepack ACCESS_TOKEN=... \
SIGNALR_CALLS=calls.json SIGNALR_OUT=frames SIGNALR_DURATION_SECS=30 \
    cargo run --example record --features messagepack-brotli
```

The doc comment at the top of the example lists all settings and output files.

## Testing your code against a fake hub

The `test-support` feature adds `signalrr::test_support`: an in-process hub on a random
local port that answers negotiate and accepts the WebSocket, and lets a test script the
server side: what negotiate returns, the handshake answer, messages to send, what the client
sent, closing or dropping the connection. No network is needed.

```toml
[dev-dependencies]
signalrr = { version = "...", features = ["test-support"] }
```

```rust
use signalrr::HubConnection;
use signalrr::test_support::FakeHub;

let hub = FakeHub::start().await;
let connection = HubConnection::builder(&hub.url).with_json_protocol().build();
let (started, mut server) = tokio::join!(connection.start(), async {
    let mut server = hub.accept().await;
    server.handshake().await;
    server
});
started?;
// server.send(...), server.recv().await, server.abort(), hub.reply_to_negotiate(...)
```

The fake hub panics on anything unexpected, which fails the test that drives it. The tests of
this crate (`tests/connect.rs`, `tests/reconnect.rs`) use it the same way.

## Development

CI (GitHub Actions) runs rustfmt, clippy with `-D warnings` and the test suite on Linux, macOS
and Windows, plus clippy and tests for each feature set. Run the same checks locally with:

```bash
./ci.sh   # needs cargo-nextest: cargo install --locked cargo-nextest
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))
