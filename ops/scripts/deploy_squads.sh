#!/usr/bin/env bash
# Guarded helper for the multisig deploy/upgrade flow. This script does NOT hold the upgrade
# authority — it only builds the buffer + prints the Squads proposal inputs. Follow
# ops/runbooks/deploy_upgrade.md. Requires Agave platform-tools.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

command -v solana >/dev/null 2>&1 || { echo "ERROR: solana-cli not installed"; exit 1; }
SO="target/deploy/arb_program.so"
[ -f "$SO" ] || { echo "ERROR: $SO missing — run 'make build-sbf' first"; exit 1; }

echo "==> writing upgrade buffer for $SO"
BUFFER=$(solana program write-buffer "$SO" | awk '/Buffer/{print $2}')
echo "buffer: $BUFFER"
echo "Now propose bpf_loader_upgradeable::upgrade from this buffer in Squads, collect"
echo "approvals, and execute. The upgrade authority is the Squads multisig, never this host."
