#!/usr/bin/env bash
# Rotate the low-balance HOT key: engage the kill-switch, sweep any residual balance to the
# cold treasury, generate a fresh 0o600 hot key, then (operator) re-arm. Treat the hot key as
# expendable (invariant §13) — rotate on a schedule and on any anomaly.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SECRETS="$ROOT/secrets"
HOT="$SECRETS/hot-keypair.json"

command -v solana >/dev/null 2>&1 || { echo "ERROR: solana-cli not installed"; exit 1; }
: "${COLD_TREASURY:?set COLD_TREASURY to the cold/multisig pubkey}"

echo "==> engaging kill-switch"
touch "$SECRETS/kill_switch"

if [ -f "$HOT" ]; then
  BAL=$(solana balance "$HOT" 2>/dev/null | awk '{print $1}' || echo 0)
  echo "==> hot balance: ${BAL} SOL — sweeping residual to cold treasury $COLD_TREASURY"
  solana transfer "$COLD_TREASURY" ALL --from "$HOT" --fee-payer "$HOT" --allow-unfunded-recipient || true
  mv "$HOT" "$HOT.retired.$(date +%s 2>/dev/null || echo old)"
fi

echo "==> generating fresh hot key"
solana-keygen new --no-bip39-passphrase --silent --outfile "$HOT"
chmod 600 "$HOT"
echo "==> new hot pubkey: $(solana-keygen pubkey "$HOT")"
echo "Fund minimally, then 'rm secrets/kill_switch' to re-arm (after root-cause if rotating on anomaly)."
