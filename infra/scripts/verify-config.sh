#!/usr/bin/env bash
# `make config-check` entrypoint. Runs arb_config::validate + the program-id ⇄ compiled-
# const cross-check (inside `validate`), exiting non-zero on any inconsistency.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

echo "==> config-check: loader::validate + program-id cross-check"
cargo run --quiet -p arb-config --bin config-check -- infra/config

# Reminder for the manual Solscan step (the scaffold provides the table + cross-check, not
# the on-chain verification itself).
if grep -q "PENDING_OPERATOR" infra/config/program_ids.toml; then
  echo "NOTE: program_ids.toml still has PENDING_OPERATOR markers — verify each id on" >&2
  echo "      Solscan and set verified_on=YYYY-MM-DD before mainnet (does not fail CI)." >&2
fi
