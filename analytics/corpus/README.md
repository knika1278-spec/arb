# Golden-replay corpus

Frozen historical arbitrage opportunities (winners **and** losers) used by the `arbit-analytics`
regression gate (`observ-10/11/12`). Each sample is replayed through the **same** bit-exact
`arb-math` mirror + `CostModel` the bot signs against, so a nonzero `predicted_out` vs
`recorded_realized_out` deviation is a real decode/sizing drift — not rounding noise.

## Schema (`GoldenSample`)

A corpus file is a JSON array of:

| field | meaning |
|---|---|
| `id` | stable sample id |
| `slot` | slot the state was captured at |
| `pool_a` / `pool_b` | `{reserve_a, reserve_b, fee_num, fee_den}` CPMM reserves (oriented `reserve_a ↔ mint_a`) |
| `dir_a` / `dir_b` | `SwapDir` tag per leg (`0` = AtoB, `1` = BtoA) |
| `amount_in` | leg-A input amount |
| `recorded_realized_out` | the realized base-out measured on-chain / in sim (balance delta) |
| `recorded_landed` | `true` for a profitable land, `false` for a reverting **loser** |
| `spread_lamports`, `swap_fees_lamports`, `tip_lamports`, `prio_lamports`, `base_lamports`, `p_land` | economic terms for the `CostModel` E[net] |

**The corpus MUST include losers** so loser-burn (base+priority on reverts) is covered — a corpus of
only winners hides the dominant cost on the thin-pool niche.

## Capture

Capture frozen state from a **Geyser snapshot** (Yellowstone `accounts` at the opportunity slot) or
**Old Faithful** historical archive: read the two pool accounts' reserves at `slot`, the route
direction, the exact `amount_in` the bot would size, and the realized output (balance delta from the
landed tx, or `simulateTransaction` `pre/postTokenBalances` for a would-be-reverting loser).

## Use

```sh
arbit-analytics gate     corpus/sample.json [tolerance_bps]   # CI gate: nonzero exit on drift
arbit-analytics replay   corpus/sample.json [tolerance_bps]   # per-sample report
arbit-analytics backtest corpus/sample.json                   # unit-economics confirmation
```

`gate` is **CI-blocking**: wire it before any capital-committing deploy. `sample.json` is a tiny
2-sample (1 winner + 1 loser) starter corpus; replace it with the real captured set.
