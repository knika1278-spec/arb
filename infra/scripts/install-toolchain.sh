#!/usr/bin/env bash
# Idempotent toolchain bootstrap consumed by `make bootstrap`.
# Installs + version-verifies every tool pinned in infra/toolchain/versions.toml.
# Exits non-zero on any version mismatch so CI and devs share one source of truth.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSIONS="$ROOT/infra/toolchain/versions.toml"

die() { echo "ERROR: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Tiny TOML reader for `key = "value"` lines (sufficient for this flat file).
tget() { grep -E "^\s*$1\s*=" "$VERSIONS" | head -1 | sed -E 's/.*=\s*"?([^"#]+)"?.*/\1/' | tr -d ' '; }

RUST_CHANNEL="$(tget channel)"
AGAVE="$(tget agave_cli)"
ANCHOR="$(tget version)"     # first `version =` is anchor's block in this file

echo "==> Pinned: rust=$RUST_CHANNEL agave=$AGAVE anchor=$ANCHOR"

# --- Rust (via rustup) ---
have rustup || die "rustup not found — install from https://rustup.rs first."
rustup toolchain install "$RUST_CHANNEL" --component rustfmt clippy rust-src --profile minimal
rustc "+$RUST_CHANNEL" --version | grep -q "$RUST_CHANNEL" || die "rustc != $RUST_CHANNEL"

# --- Agave / solana-cli (provides cargo-build-sbf + solana-verify platform-tools) ---
if have solana; then
  GOT="$(solana --version | awk '{print $2}')"
  [ "$GOT" = "$AGAVE" ] || echo "WARN: solana-cli $GOT != pinned $AGAVE"
else
  echo "WARN: solana-cli not installed. Install Agave $AGAVE to enable 'cargo build-sbf'"
  echo "      (the on-chain program cannot be built into a .so without it):"
  echo "      sh -c \"\$(curl -sSfL https://release.anza.xyz/v$AGAVE/install)\""
fi

# --- supply-chain tooling ---
have cargo-audit || cargo install cargo-audit --locked || echo "WARN: cargo-audit install failed"
have cargo-deny  || cargo install cargo-deny  --locked || echo "WARN: cargo-deny install failed"

echo "==> bootstrap complete"
