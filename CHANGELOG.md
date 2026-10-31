# Changelog

All notable changes to this crate. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.0-alpha] - 2026-10-31

First tagged release. Not published on crates.io yet: depend on the git tag. The API is not
stable.

### Added

- HTTP negotiate with redirects and `connectionToken`; one retry on `401` with a fresh access
  token.
- WebSocket transport with an ordered send queue; TLS through `native-tls`.
- JSON and MessagePack v2 hub protocols, the custom `messagepack+brotli` protocol (feature
  `messagepack-brotli`).
- `invoke`, `send`, `on`, `on_async`, `off`; typed handlers `on_typed` that deserialize
  straight from the received MessagePack bytes.
- Streaming both ways: `stream()` and client-to-server stream arguments.
- Automatic reconnect with a `RetryPolicy`, stateful reconnect with `Ack` and `Sequence`
  that resumes the same connection over a new WebSocket.
- Keep-alive ping and server timeout, lifecycle callbacks, handshake version selection.
- In-process fake hub for tests of code built on this client (feature `test-support`).
- Example that records raw hub frames.

### Fixed

- The access token is put into the WebSocket URL once and kept out of logged URLs.
- The WebSocket opens at the URL negotiate redirected to.
- The ping timer counts from sent messages, the server timeout from received ones.
- `close()` aborts a `start()` that waits for the server.

[0.1.0-alpha]: https://github.com/san-smith/signalrr/releases/tag/v0.1.0-alpha
