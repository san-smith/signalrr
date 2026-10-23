#!/usr/bin/env bash
# Runs the same checks as .github/workflows/ci.yml on the current machine.
# Requires cargo-nextest (`cargo install --locked cargo-nextest`).
set -euo pipefail
cd "$(dirname "$0")"

step() { printf '\n==> %s\n' "$*"; }

step "fmt"
cargo fmt --all -- --check

step "clippy (all features)"
cargo clippy --all-targets --all-features -- -D warnings

step "nextest (all features)"
cargo nextest run --all-features

step "doc tests (all features)"
cargo test --doc --all-features

feature_sets=(
    "--no-default-features --features json"
    "--no-default-features --features messagepack"
    "--no-default-features --features messagepack-brotli"
    ""
)
for features in "${feature_sets[@]}"; do
    step "feature set: ${features:-default}"
    # shellcheck disable=SC2086
    cargo clippy --all-targets $features -- -D warnings
    # shellcheck disable=SC2086
    cargo test $features
done

step "ok"
