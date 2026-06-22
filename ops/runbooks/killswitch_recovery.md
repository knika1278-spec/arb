# Runbook — kill-switch trip & recovery

The kill-switch is a file the signer checks **before every sign** (`secrets/kill_switch`).
Presence ⇒ all signing halts within one check cycle. It is tripped manually or automatically
on health-metric breaches.

## Concrete trip thresholds (Fase 2 — tune from live data)

| Signal | Threshold | Source |
|---|---|---|
| Revert-rate | > 30% over 5 min | observability `HealthEvaluator` (`>30% = infra bug`) |
| Burn-rate (lamports/min on reverted losers) | > operator cap | observability `econ`/`burn` |
| Realized loss | > Y SOL / hour | `PnlLedger` |
| Hot-key balance deviation | > Z from expected | signer pre-sign snapshot |
| Synchronous pre-sign cap | per-interval count + cumulative lamport-out | signer `PreSignCaps` (local, no round-trip) |

> The metric-based trips lag (status polling is seconds). They are paired with the
> **synchronous local pre-sign cap** in the signer so worst-case outflow per window is
> bounded BEFORE the lagging metrics catch up.

## Manual halt (seconds)

```bash
touch secrets/kill_switch        # signer refuses to sign / start immediately
# optionally also revoke the Jito auth UUID and stop the bot process
```

## Recovery

1. **Stop** the bot + signer processes.
2. **Diagnose**: pull the dashboard (revert-rate, burn-rate, last N `RevertCause`). Revert-rate
   > 30% almost always means an infra bug (stale cache, bad blockhash loop, wrong CU limit),
   not strategy — fix the root cause, do not just raise the cap.
3. **Sweep** surplus from the hot key to cold treasury if balance deviated.
4. **Rotate** the hot key if compromise is suspected (`ops/scripts/rotate_hot_key.sh`).
5. **Re-arm**: remove the kill-switch file only after the root cause is fixed and a dry-run on
   Surfpool/mainnet-fork passes.

```bash
rm secrets/kill_switch
```

6. **Post-mortem**: record the cause + the threshold that should have caught it earlier.
