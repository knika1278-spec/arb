#!/usr/bin/env bash
# Launch surfpool as a headless lazy mainnet-fork and wait until its RPC is healthy, then
# export the endpoint for the surfpool integration tests (testing-8 / onchain-11).
#
# Surfpool clones mainnet accounts ON DEMAND (lazy fork), so the REAL Raydium CPMM / Orca
# Whirlpool programs + pool accounts appear in the simnet the moment a tx references them — no
# explicit snapshot needed. The datasource RPC is the keyed Chainstack node when provisioned
# (SURFPOOL_DATASOURCE_RPC_URL), else the public mainnet-beta endpoint.
#
# Usage:
#   bash tests/scripts/run_surfpool.sh [--wait-only]
#     (default) launch in the background, wait for health, print PID + endpoint, stay up.
#     --wait-only: assume an instance is already running; just wait for health.
#
# Env:
#   SURFPOOL_PORT          RPC port (default 8899)
#   SURFPOOL_DATASOURCE_RPC_URL   mainnet datasource (default: public mainnet-beta)
#   SURFPOOL_BIN           surfpool binary (default: surfpool on PATH)
set -uo pipefail

PORT="${SURFPOOL_PORT:-8899}"
URL="http://127.0.0.1:${PORT}"
BIN="${SURFPOOL_BIN:-surfpool}"
RPC="${SURFPOOL_DATASOURCE_RPC_URL:-}"
LOG="${SURFPOOL_LOG:-/tmp/surfpool-fork.log}"

health() {
  curl -s --max-time 3 "$URL" -X POST -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' 2>/dev/null
}

wait_healthy() {
  for _ in $(seq 1 30); do
    case "$(health)" in
      *'"result":"ok"'*) echo "surfpool healthy at ${URL}"; return 0 ;;
    esac
    sleep 1
  done
  echo "ERROR: surfpool did not become healthy at ${URL} within 30s" >&2
  return 1
}

if [ "${1:-}" = "--wait-only" ]; then
  wait_healthy
  exit $?
fi

# Launch headless. Use the configured datasource RPC if set, else the predefined mainnet.
if [ -n "$RPC" ]; then
  SRC=(--rpc-url "$RPC")
else
  SRC=(-n mainnet)
fi

echo "launching: $BIN start --no-tui --no-deploy ${SRC[*]} -p ${PORT}  (log: $LOG)"
( cd /tmp && "$BIN" start --no-tui --no-deploy "${SRC[@]}" -p "$PORT" >"$LOG" 2>&1 ) &
PID=$!
echo "surfpool pid: $PID"
wait_healthy || { echo "--- log tail ---"; tail -20 "$LOG"; exit 1; }
echo "SURFPOOL_RPC=${URL}"
