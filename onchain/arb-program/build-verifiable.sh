#!/usr/bin/env bash
# Reproducible build of the on-chain program. Emits the verifiable bytecode hash so the
# deployed program can be proven to match this source (invariant §14). Requires the pinned
# Agave platform-tools + solana-verify.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

if ! command -v solana-verify >/dev/null 2>&1; then
  echo "ERROR: solana-verify not installed (cargo install solana-verify)" >&2
  exit 1
fi

# Build inside a pinned container for bit-reproducibility, then print the executable hash.
solana-verify build --library-name arb_program
echo "---- verifiable hash ----"
solana-verify get-executable-hash target/deploy/arb_program.so
