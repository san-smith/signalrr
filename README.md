# signalrr

[![crates.io](https://img.shields.io/crates/v/signalrr.svg)](https://crates.io/crates/signalrr)
[![docs.rs](https://img.shields.io/docsrs/signalrr)](https://docs.rs/signalrr)

Rust client for **ASP.NET Core SignalR** with first-class support for:

- ✅ MessagePack protocol
- ✅ Server-to-client streaming
- ✅ Event subscriptions (`on`)
- ✅ Optional Brotli/Gzip compression
- ✅ Compilation to WebAssembly (WASM)

## Status

> 🚧 **Work in progress** — not yet published on crates.io.

This library is under active development. The API is **not stable**.

## Features

- `compression` — enable Brotli/Gzip payload compression (requires compatible server)
- `tracing` — structured logging (WASM support via `tracing-wasm`)

## Basic usage

```rust
use signalrr::SignalRClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SignalRClient::builder("http://localhost:5000/chathub")
        .build()
        .await?;

    client.start().await?;
    println!("Connected!");

    Ok(())
}
```

## Compatibility

- ✅ ASP.NET Core SignalR (with AddMessagePackProtocol())
- ❌ Classic SignalR (.NET Framework)
- ⚠️ Compression only works with custom or extended SignalR servers

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))
