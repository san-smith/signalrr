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

Planned next:

- streaming hub invocations

## Example

```rust
use signalrr::HubConnection;

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
