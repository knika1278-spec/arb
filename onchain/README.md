# `onchain/arb-program` — native-Rust `TryArbitrage`

Single-instruction atomic arbitrage program (hot path; **not** Anchor). See `src/lib.rs` for
module layout and `plan.md` §6 / `implementation-plan.md` §5.2 for the design.

## What is implemented (host-compiled + unit-tested)

- **Instruction ABI** (`instruction.rs`): fixed-size LE layout, `unpack`/`pack` roundtrip.
- **Trust boundary** (`trust.rs`): swap-CPI target must be allowlisted; balance-read accounts
  must be owned by the bot authority; authority must sign.
- **Zero-copy balance reads** (`state.rs`): SPL/Token-2022 `amount`@64, `owner`@32.
- **Token-2022 vetting** (`token2022.rs`): guarded TLV scan, HARD-REJECT matrix.
- **Processor** (`processor.rs`): snapshot → CPI A → measured delta → CPI B → terminal
  profit-assert (`post_base >= pre_base + min_profit`, else `Err(Unprofitable)` → runtime
  reverts ALL state).
- **Allowlist** (`allowlist.rs`): delegates to the shared `arb-config` no_std const table.

## What is NOT yet done (needs Agave platform-tools / `cargo build-sbf`)

This environment has **no `solana-cli` / `cargo-build-sbf` / SBF target**, so the program
cannot be compiled into a deployable `.so` here, and the on-chain integration cannot run.

1. **Per-venue CPI discriminators** in `adapters/*` are **UNVERIFIED PLACEHOLDERS** (`[0;8]`).
   Fill each from the venue IDL (Anchor `sha256("global:<ix>")[..8]`) and the canonical
   account order, then prove with the M1-GATE differential.
2. **`build-sbf`**: `make build-sbf` → `cd onchain/arb-program && cargo build-sbf`.
3. **M1-GATE** (`tests/differential_rounding.rs`): off-chain `arb-math` predicted output ==
   on-chain CPI realized output, bit-exact, per-venue, both directions, incl. Token-2022
   fee path. **No mainnet send before this is green.**
4. **Revert proof** on LiteSVM + Surfpool mainnet-fork (deliberately-unprofitable input →
   `FailedTransactionMetadata`, no net token movement).
5. **`declare_id!`** + verifiable build (`build-verifiable.sh`, `solana-verify`), upgrade
   authority in Squads multisig.

## Build (when platform-tools are installed)

```bash
make build-sbf          # produces target/deploy/arb_program.so
solana-verify build     # reproducible hash (see build-verifiable.sh)
```
