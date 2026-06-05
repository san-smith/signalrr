# signalrr

[![docs.rs](https://img.shields.io/docsrs/signalrr)](https://docs.rs/signalrr)

Rust client for **ASP.NET Core SignalR** with support for MessagePack and custom hub protocols.

## Status

> Work in progress — API is not stable.

Implemented:

- text/binary message framing
- JSON handshake
- HTTP negotiate with redirects and `connectionToken`
- WebSocket transport
- `HubConnection::start()` / `close()` lifecycle
- hub message model (`HubMessage`, invocation/completion/ping/…)
- JSON and MessagePack v2 hub protocols (`HubProtocol`)
- builder helpers: `with_json_protocol()`, `with_messagepack_protocol()`
- `invoke` / `send` / `on` hub methods
- server timeout (30s) and keepalive ping (15s)
- custom `messagepack+brotli` hub protocol (feature `messagepack-brotli`)
- automatic reconnect with configurable [`RetryPolicy`](https://docs.rs/signalrr/latest/signalrr/trait.RetryPolicy.html)
- stateful reconnect with message buffering (`with_stateful_reconnect()`)
- streaming hub invocations (`stream()` / `StreamInvocation`)
- automatic handshake version selection (legacy v1 without stateful reconnect, hub protocol v2 with stateful reconnect)
- lifecycle callbacks: `on_close`, `on_reconnecting`, `on_reconnected`
- pending invocation cancellation on disconnect and server `CloseMessage` handling
- multiple client handlers per hub method (`on` / `on_async`, `off` / `off_handler`)
- client-to-server streaming hub method arguments (`HubArgument::Stream`)

Planned next:

- HttpConnection infrastructure (transport send queue, injectable HTTP client, logging)

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

## Development

```bash
cargo test
cargo test --features messagepack-brotli
cargo fmt
cargo clippy
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))
