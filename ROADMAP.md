# Roadmap

Planned work, in order. The first consumer is a desktop betting shop client that talks to an
ASP.NET Core hub over `messagepack+brotli`; items are ordered by what it needs first.
The crate is developed, not rewritten.

## State (2026-10-23)

- About 7.5k lines, edition 2024, builds clean on stable 1.92, no `unsafe`, no `unwrap` or
  `expect` in library code (denied by clippy).
- 99 unit tests and 1 doc test pass with `--all-features`; every feature set builds and
  passes clippy on its own.
- CI on GitHub Actions: rustfmt, clippy and tests on Linux, macOS and Windows; `./ci.sh` runs
  the same checks locally.
- Protocol coverage: negotiate with redirects, JSON and MessagePack v2, `messagepack+brotli`
  (compression flag at position 4, payload at 5), automatic and stateful reconnect with
  buffering and `Ack`/`Sequence`, streaming both ways, keep-alive, handshake version
  selection, callbacks, send queue, access token factory, custom headers.
- TLS goes through `native-tls` for both WebSocket and HTTP, so that
  `cargo check --target x86_64-pc-windows-msvc` works on Linux and macOS without a C
  toolchain for that target.

## Next

1. ~~**Clippy and CI.**~~ Done 2026-10-23: clippy is clean, CI runs fmt, clippy and tests,
   the `json`-only build is fixed.
2. **Smoke test against a real hub** with `messagepack+brotli`: connect, invoke a method,
   subscribe, receive a large snapshot. Save the raw frames as fixtures for `insta`
   snapshot tests. The test is opt-in (ignored by default, endpoint from an environment
   variable), so the default test run needs no network.
3. **Typed receive.** Handlers get `Vec<serde_json::Value>` today: MessagePack goes through
   `rmpv`, then a JSON value, then the target struct. For a snapshot of ~1.5k entities this
   is a double conversion. Add `on_typed::<T: DeserializeOwned>` that deserializes straight
   from the protocol representation.
4. **Integration harness.** An in-process fake hub on `tokio_tungstenite::accept_async`
   covering negotiate, handshake, connection drop, reconnect and stateful reconnect. Only
   unit tests exist now.
5. **`HubConnection` refactor.** Eight `Arc<RwLock<Option<...>>>` fields become one state
   behind one lock, or commands sent to a runtime task. A good task for an external
   reviewer.
6. **Release prep.** Update dependencies (`tokio-tungstenite`, `thiserror` 2, `brotli`),
   complete rustdoc, update the README, release `0.1.0-alpha` on crates.io. Publishing
   waits for a security sign-off from the first consumer's team.
