#!/usr/bin/env bash
# Thin wrapper so `make verify-build` and ops share one command.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exec bash "$ROOT/onchain/arb-program/build-verifiable.sh"
