# signalrr

[![docs.rs](https://img.shields.io/docsrs/signalrr)](https://docs.rs/signalrr)

Rust client for **ASP.NET Core SignalR** with support for MessagePack and custom hub protocols.

## Status

> Work in progress — API is not stable.

Current scope (v0 foundation):

- text/binary message framing
- JSON handshake
- HTTP negotiate with `connectionToken`
- `HubConnection` builder skeleton
- internal `MessageBus` for invoke/stream dispatch

Planned next:

- WebSocket transport + `HubConnection` lifecycle
- MessagePack hub protocol v2
- custom `messagepack+brotli` protocol
- automatic and stateful reconnect

## Example

```rust
use signalrr::HubConnection;

fn main() {
    let connection = HubConnection::builder("https://example.com/signalr")
        .with_stateful_reconnect(true)
        .build();

    assert_eq!(connection.state(), signalrr::HubConnectionState::Disconnected);
}
```

## Development

```bash
cargo test
cargo fmt
cargo clippy
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))
