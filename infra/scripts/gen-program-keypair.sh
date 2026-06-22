#!/usr/bin/env bash
# Generate (a) the upgradeable program keypair and (b) a throwaway low-balance HOT keypair,
# both chmod 600 under /secrets (never committed). Treasury/upgrade authority is NOT
# generated here — it lives in KMS + a Squads multisig (invariant §12/§14).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SECRETS="$ROOT/secrets"
mkdir -p "$SECRETS"

have() { command -v "$1" >/dev/null 2>&1; }
have solana-keygen || { echo "ERROR: solana-keygen not found (install Agave CLI)"; exit 1; }

PROGRAM_KEY="$SECRETS/program-keypair.json"
HOT_KEY="$SECRETS/hot-keypair.json"

if [ ! -f "$PROGRAM_KEY" ]; then
  solana-keygen new --no-bip39-passphrase --silent --outfile "$PROGRAM_KEY"
  chmod 600 "$PROGRAM_KEY"
  echo "==> program keypair: $PROGRAM_KEY ($(solana-keygen pubkey "$PROGRAM_KEY"))"
  echo "    -> set declare_id! and infra/config/program_ids.toml to this pubkey"
else
  echo "==> program keypair already exists; leaving untouched"
fi

if [ ! -f "$HOT_KEY" ]; then
  solana-keygen new --no-bip39-passphrase --silent --outfile "$HOT_KEY"
  chmod 600 "$HOT_KEY"
  echo "==> hot keypair: $HOT_KEY ($(solana-keygen pubkey "$HOT_KEY")) — keep balance MINIMAL"
else
  echo "==> hot keypair already exists; leaving untouched"
fi

# Final guard: prove nothing under secrets/ is tracked.
if git -C "$ROOT" ls-files --error-unmatch "secrets/hot-keypair.json" >/dev/null 2>&1; then
  echo "FATAL: hot-keypair.json is tracked by git — .gitignore is broken!" >&2
  exit 2
fi
echo "==> OK: keypairs are 0o600 and gitignored"
