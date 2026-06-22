#!/usr/bin/env bash
# Verify that every git-pinned dependency's locked commit matches the integrity hash we
# recorded in integrity-hashes.lock (invariant §11 "integrity hash"). Fails if a git-pinned
# rev changed without an explicit update here — the supply-chain tripwire beyond Cargo.lock.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOCKFILE="$ROOT/Cargo.lock"
PINS="$ROOT/infra/audit/integrity-hashes.lock"

[ -f "$LOCKFILE" ] || { echo "ERROR: Cargo.lock missing"; exit 1; }
[ -f "$PINS" ] || { echo "ERROR: $PINS missing"; exit 1; }

fail=0
# Each non-comment line: "<crate> <git-rev-sha>"
while read -r name rev _; do
  case "$name" in ''|\#*) continue;; esac
  # Find the source line for this crate in Cargo.lock; extract the ?rev=... / #<sha> commit.
  got="$(grep -A3 "^name = \"$name\"\$" "$LOCKFILE" | grep -Eo '#[0-9a-f]{40}' | tr -d '#' | head -1 || true)"
  if [ -z "$got" ]; then
    echo "  - $name: not a git source in Cargo.lock (skipping)"
    continue
  fi
  if [ "$got" != "$rev" ]; then
    echo "FAIL: $name git rev $got != pinned $rev" >&2
    fail=1
  else
    echo "  ok: $name @ $rev"
  fi
done < "$PINS"

[ "$fail" -eq 0 ] || { echo "integrity check FAILED" >&2; exit 1; }
echo "==> integrity OK"
