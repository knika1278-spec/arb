# Implementation Plan — Solana Atomic Arbitrage (Milestone 1)

> **Executable build plan derived from [`plan.md`](./plan.md)** (the canonical design spec). `plan.md` is *what & why*; this document is *how, in what order, and how we know each piece is done*. It defines the repo layout, nine module designs (files, public interfaces, data structures), a **107-task DAG** with a 25-node critical path, phase sequencing (Fase 0→1→2 in detail; 3→4 summarized), a cross-cutting risk register, and a **critique-driven addendum** of tasks that must be added before any mainnet capital.

This plan was produced by decomposing `plan.md` across nine engineering workstreams, synthesizing a unified DAG, and running an adversarial completeness pass against the spec's §12 *Definition of Done*. Sections that carry that adversarial review are flagged **[critique]**.

---

## 0. How to use this document

- **Audience:** the engineer(s)/agents building the system. Each task is self-contained enough to pick up in isolation given the spec section it cites.
- **Task IDs:** `<module>-<n>` (e.g. `onchain-8`). IDs are stable; the DAG, critical path, phase lists, and risk register all reference them. Added tasks from the completeness review use the `add-*` / `dec-*` namespace (§9).
- **`Done when`** lines are the acceptance criteria — a task is not complete until every bullet is demonstrably true (test, measurement, or artifact). Prefer automated checks.
- **The single hard gate is `M1-GATE`** (§7): off-chain predicted output must equal on-chain realized output, bit-exact, per-venue, both directions, including the Token-2022 fee path. **No mainnet send before it is green.**
- **Estimates** are solo-senior-dev days; phase durations assume aggressive parallelism across module tracks (most tasks depend only on the shared scaffold). The realistic, re-reconciled durations are in §8 — they are materially longer than `plan.md` §11's headline numbers; trust the re-estimates.
- **Gate phases on exit-criteria, not the calendar.**

---

## 1. Scope & Milestone-1 definition

**In scope (Milestone 1):** a single-transaction, all-or-nothing **atomic** arbitrage between two fully-decodable constant-product venues, with an on-chain profit-assertion that makes the runtime revert the entire transaction when the trade is not profitable. Pre-funded WSOL/USDC inventory (no flash loan). Wave-1 venues: **Raydium CPMM**, **Orca Whirlpool**, and **PumpSwap AMM** (added in Fase 2 to capture pump.fun graduations).

**Out of scope (deferred):** Raydium AMM v4 legacy (Wave 2), Phoenix CLOB (Fase 3), flash loans, prop-AMM decoding, non-atomic CEX/DEX latency arb, sandwich/frontrun.

> ⚠️ **SCOPE EXPANSION (added 2026-06-22, explicit user opt-in after on-chain audit).** Three items previously
> deferred are promoted to **active** in `TODO.md` → "Fase 2.5", on the evidence that the largest observed
> mainnet opportunities live there (ANB intra-Meteora; elun dex-to-dex sell leg on Raydium CLMM):
> **(a) Meteora DLMM + DAMM v2 adapters/quoters** (`onchain-17/18`, `sizing-12/13`),
> **(b) Raydium CLMM adapter/quoter** (`onchain-19`, `sizing-14`),
> **(c) triangular / multi-hop** (`onchain-20` N-leg processor, `sizing-15`, `detection-12`, `txbuilder-15`).
> This **changes the Definition of Success below**: criterion 1's "2 swap CPI" becomes "2-or-3 swap CPI"
> for the triangle path, and the math layer adds cycle-based sizing alongside the 2-pool closed form. The hard
> gate is unchanged in spirit but **extended**: `M1-GATE-EXT` requires per-venue, both-direction bit-exact
> differential GREEN for each new venue before any mainnet send. Conflicts with the "follow-TODO-strictly"
> directive; recorded only because the user requested it explicitly. Original 2-swap M1 remains the
> recommended first-land path (see the NICHE-COVERAGE go/no-go in `TODO.md`).

**Definition of Success (from `plan.md` §1, mapped to tasks in §11):**
1. Native-Rust program with a single `TryArbitrage` instruction: 2 swap CPI + terminal profit-assert that returns `Err(Unprofitable)` when `out < in + min_profit + costs`.
2. Proven to revert on intentionally-unprofitable input (LiteSVM + mainnet-fork), with no net token movement.
3. Lands **profitable** at least once on mainnet at small size via a Jito bundle, tip **inside** the same atomic tx.
4. Detection latency instrumented (Yellowstone gRPC); **revert-rate** and **burn-rate** are first-class health metrics.

---

## 2. Load-bearing invariants (non-negotiable across all modules)

1. **Atomicity is a runtime property.** A terminal `Err` reverts ALL state; the on-chain assert is just the gate. Pre-funded inventory, not flash loan, for M1.
2. **On-chain assert is the *only* real safety net.** It must hold even with `skipPreflight=true`. Never rely on preflight-fail for fee protection.
3. **Hot path is native Rust** (`solana-program`), not Anchor. Anchor only for tooling/prototyping.
4. **v0 transactions + pre-warmed ALT from day 1.** **Never** extend-then-use an ALT in the same slot.
5. **Hard limits:** `MAX_TX_ACCOUNT_LOCKS=128` (the binding ceiling, not 256), tx **≤1232 bytes** (ALT does *not* raise the byte cap), `MAX_COMPUTE_UNIT_LIMIT=1.4M` CU/tx, ≤256 loaded accounts. Signers cannot live in an ALT.
6. **Trust boundary:** pool accounts arrive **untrusted** via `remaining_accounts`. The program verifies (a) every swap-CPI target is an allowlisted DEX program id, (b) balance-read token-accounts are owned by the bot authority. Mirrored in the signer's tx-shape validator and the tx-builder vetting — single source `onchain/allowlist.rs`.
7. **Profit-check from *actual balance delta*** (read ATA amount pre/post each leg), never the instruction `amount`. Token-2022 transfer-fee skims the received amount.
8. **Token-2022 HARD-REJECT** mints with `TransferHook(non-null)` / `NonTransferable` / `frozen` / `MemoTransfer` / `ConfidentialTransfer` / `PermanentDelegate` / `MintCloseAuthority`. Allow plain SPL + fee-only Token-2022.
9. **Off-chain integer math mirrors on-chain bit-exact** (Floor output, Ceil required-input). Milestone 1 is **gated** on the per-venue, both-direction fuzz/property test (`M1-GATE`).
10. **Tip transfer lives _inside_ the same atomic tx.** (fail ⇒ tip unpaid) + `jitodontfront`; routing exclusive via Jito. Helius Sender / SWQoS only as explicit fallback.
11. **Idempotent detection cache** keyed by pool pubkey; dedupe by `(slot, write_version)` **intra-session only**; on reconnect prefer the higher slot unconditionally. `write_version` is incomparable across sessions/failover.
12. **Key management:** in-process ed25519 signing sidecar holds **only** a small-balance hot key; treasury/upgrade authority in KMS + Squads multisig; signer enforces synchronous pre-sign caps + tx-shape validation + kill-switch. Async sweeper caps blast radius.
13. **Health metrics are first-class:** **revert-rate** (>30% ⇒ infra bug) and **burn-rate** (lamports/min on reverted losers) feed the kill-switch alongside latency P50/P95 and PnL.
14. **Deploy posture:** upgradeable program, upgrade authority in Squads multisig, published verifiable build (`solana-verify`).

> These are encoded as compile-time constants in `crates/arb-config` (the Wave-1 allowlist + the hard limits) so the on-chain verifier and the off-chain builder cannot drift apart.

---

## 3. Repo & workspace layout

A single cargo workspace. `crates/arb-config` (no_std-capable) and `crates/arb-types` are the **single source of truth** every other crate links — the program-id allowlist and hard limits exist once and are shared by the on-chain program and the off-chain bot.

```text
arbit/                                  # cargo workspace root (scaffold module)
├── Cargo.toml                          # [workspace] + centralized pinned [workspace.dependencies]
├── Cargo.lock                          # committed
├── rust-toolchain.toml                 # pinned channel (Agave-matched)
├── rustfmt.toml
├── clippy.toml
├── Makefile
├── README.md
├── .gitignore                          # secrets guard
├── .env.example
├── .github/
│   └── workflows/
│       ├── ci.yml                      # build/lint/test/lockfile/audit/config gates
│       └── verifiable-build.yml        # solana-verify reproducible build
│
├── crates/                             # shared library crates (source of truth for all)
│   ├── arb-config/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── program_ids.rs          # WAVE1_DEX_ALLOWLIST, is_allowlisted_swap_program (no_std)
│   │       ├── limits.rs               # MAX_TX_ACCOUNT_LOCKS=128, TX_SIZE_LIMIT_BYTES=1232, etc (no_std)
│   │       ├── providers.rs            # ArbConfig::active_landing, landing ladder (std)
│   │       ├── secrets.rs              # load_hot_keypair, kill_switch_engaged (std)
│   │       └── loader.rs               # load(), validate() (std)
│   ├── arb-types/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs                  # ArbError (repr u32, 6000-base), DexKind
│   └── arb-math/                       # compile-stub here; real content owned by sizing module
│       └── Cargo.toml
│
├── onchain/                            # native-Rust TryArbitrage program (onchain module)
│   ├── Cargo.toml
│   ├── build-verifiable.sh
│   ├── README.md
│   ├── src/
│   │   ├── lib.rs
│   │   ├── entrypoint.rs               # solana-nostd-entrypoint hot-path
│   │   ├── instruction.rs              # TryArbitrageData::unpack, Dex/LegDescriptor
│   │   ├── processor.rs                # snapshot->CPI A->delta->CPI B->terminal assert
│   │   ├── error.rs                    # ArbitrageError (repr u32)
│   │   ├── allowlist.rs                # is_allowlisted_dex (authoritative; infra mirrors)
│   │   ├── trust.rs                    # verify_swap_program, verify_balance_account
│   │   ├── state.rs                    # read_token_amount (zero-copy offset 64)
│   │   ├── token2022.rs                # vet_mint (HARD-REJECT extension matrix)
│   │   ├── constants.rs
│   │   └── adapters/
│   │       ├── mod.rs                  # SwapAdapter trait, dispatch
│   │       ├── raydium_cpmm.rs
│   │       ├── orca_whirlpool.rs       # swap_v2
│   │       └── pumpswap.rs             # Fase 2 venue
│   └── tests/
│       ├── common/mod.rs
│       ├── litesvm_revert.rs
│       ├── token2022_filter.rs
│       └── rounding_mirror_fuzz.rs     # imports sizing math (cross-module fuzz gate)
│
├── bot/                                # off-chain hot path (multiple modules)
│   ├── arb-bot/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── math/                   # sizing/math module
│   │       │   ├── mod.rs
│   │       │   ├── u256.rs
│   │       │   ├── mul_div.rs          # mul_div_floor/ceil
│   │       │   ├── rounding.rs         # RoundDirection
│   │       │   └── fees.rs             # transfer_fee_forward/inverse
│   │       ├── sizing/                 # sizing module
│   │       │   ├── mod.rs
│   │       │   ├── quoter.rs           # Quoter trait
│   │       │   ├── venues/{mod,raydium_cpmm,orca_whirlpool,pumpswap}.rs
│   │       │   ├── roundtrip.rs        # RoundTrip::realized_out, CpmmReserves
│   │       │   ├── cpmm_optimal.rs     # optimal_delta_general/fee30bps
│   │       │   ├── opportunity.rs      # opportunity_exists
│   │       │   ├── policy.rs           # 90-95% sizing policy
│   │       │   ├── search.rs           # golden_section_max (fase3)
│   │       │   ├── cycle.rs            # find_negative_cycle (fase3)
│   │       │   ├── error.rs
│   │       │   ├── fixtures/mod.rs
│   │       │   └── tests/differential.rs   # consumes onchain CPI harness realized_out
│   │       ├── detection/              # detection module
│   │       │   ├── mod.rs
│   │       │   ├── config.rs
│   │       │   ├── model.rs            # SessionStamp, DetectionSignal, EdgeUpdate
│   │       │   ├── grpc.rs             # Yellowstone ingest
│   │       │   ├── decode.rs           # PoolDecoder per venue
│   │       │   ├── cache.rs            # PoolStateCache::apply, accept_predicate
│   │       │   ├── graph.rs            # PairGraph::on_event
│   │       │   ├── reconnect.rs        # RpcReconciler supervisor
│   │       │   ├── discovery.rs        # PoolDiscovery (fase3 seam)
│   │       │   ├── metrics.rs
│   │       │   └── tests/mod.rs
│   │       ├── txbuilder/              # txbuilder module
│   │       │   ├── mod.rs
│   │       │   ├── builder.rs          # build_arb_tx
│   │       │   ├── layout.rs           # canonical instruction layout
│   │       │   ├── limits.rs           # validate (locks<128, bytes<=1232)
│   │       │   ├── compute.rs          # ComputeBudgetParams::from_measured
│   │       │   ├── wsol.rs             # WsolDance::wrap_legs
│   │       │   ├── token2022.rs        # Token2022Filter::vet_mint (mirrors onchain)
│   │       │   ├── preflight.rs        # simulate, profit_from_balances
│   │       │   ├── tip.rs              # Jito tip ix (fase2 seam)
│   │       │   ├── config.rs
│   │       │   ├── error.rs
│   │       │   └── tests/{limits_tests,token2022_tests,wsol_tests}.rs
│   │       ├── executor/               # landing/executor module
│   │       │   ├── mod.rs
│   │       │   ├── facade.rs           # Executor::land
│   │       │   ├── bundle_build.rs     # tip-inside-atomic + jitodontfront
│   │       │   ├── jito.rs             # JitoClient (send_bundle, tip_accounts, simulate_bundle)
│   │       │   ├── regions.rs
│   │       │   ├── tip.rs              # TipOracle::size_tip/next_tip_account
│   │       │   ├── sim.rs              # simulate_for_profit
│   │       │   ├── landing_loop.rs     # run_landing_loop (fresh-blockhash rebuild)
│   │       │   ├── sender.rs           # HeliusSender::send_fast
│   │       │   ├── swqos.rs            # SwqosSender::send_staked
│   │       │   ├── metrics.rs          # ExecutorMetrics
│   │       │   ├── types.rs
│   │       │   ├── config.rs / config/executor.toml
│   │       │   └── tests/{landing_loop_tests,tip_sizing_tests}.rs
│   │       ├── signer/                 # signer sidecar module
│   │       │   ├── mod.rs
│   │       │   ├── sidecar.rs          # SignerSidecar::sign_arb_tx
│   │       │   ├── keychain.rs         # SolanaSigner trait, MemorySigner
│   │       │   ├── validate.rs         # TxShapeValidator::validate
│   │       │   ├── caps.rs             # PreSignCaps::reserve/apply_snapshot
│   │       │   ├── killswitch.rs       # KillSwitchHandle
│   │       │   ├── thresholds.rs       # KillSwitchSupervisor::evaluate
│   │       │   ├── sweeper.rs          # Sweeper::run -> cold treasury
│   │       │   ├── health.rs / alert.rs / error.rs / metrics.rs / config.rs
│   │       │   └── tests/{shape_reject,caps_killswitch}.rs
│   │       └── metrics/                # observability module
│   │           ├── mod.rs
│   │           ├── registry.rs         # MetricsRegistry
│   │           ├── counters.rs / latency.rs / slippage.rs
│   │           ├── pnl.rs              # PnlLedger
│   │           ├── health.rs           # HealthEvaluator
│   │           ├── econ.rs             # CostModel::e_net/gate, PLandEstimator
│   │           ├── exporter.rs         # Prometheus /healthz
│   │           ├── alerts.rs           # AlertRouter::dispatch
│   │           └── config.rs
│   └── arb-signer/                     # separate-process signer binary (optional sidecar)
│       ├── Cargo.toml
│       └── src/main.rs
│
├── infra/                              # config, ALT tooling, supply-chain (scaffold + txbuilder)
│   ├── config/
│   │   ├── program_ids.toml            # mirrors onchain/allowlist.rs
│   │   ├── providers.toml
│   │   └── limits.toml
│   ├── toolchain/versions.toml
│   ├── scripts/
│   │   ├── install-toolchain.sh
│   │   ├── verify-config.sh            # program-id Solscan cross-check
│   │   └── gen-program-keypair.sh
│   ├── deny.toml
│   ├── audit/                          # integrity-hashes.lock / cargo-vet
│   └── alt/                            # ALT lifecycle (txbuilder module)
│       ├── mod.rs
│       ├── manager.rs                  # AltManager::ensure_keys_present/extend
│       ├── warmup.rs                   # is_warm (never extend-then-use same slot)
│       ├── janitor.rs                  # AltJanitor::try_close (~512 slots)
│       ├── static_set.rs
│       ├── config.toml
│       └── tests/alt_lifecycle_tests.rs
│
├── analytics/                          # backtest / golden-replay CLI (observability module)
│   ├── Cargo.toml
│   ├── src/{main,corpus,replay,backtest,report}.rs
│   ├── dashboards/grafana-arbit-health.json
│   └── corpus/README.md
│
├── ops/                                # deploy/ops/runbooks (signer module)
│   ├── config/{signer.toml,killswitch.toml}
│   ├── deny.toml
│   ├── scripts/{verify_build.sh,deploy_squads.sh,rotate_hot_key.sh}
│   ├── runbooks/{killswitch_recovery.md,deploy_upgrade.md}
│   └── SUPPLY_CHAIN.md
│
├── secrets/                            # gitignored; README only committed
│   └── README.md
│
└── tests/                              # cross-module integration harness (testing module)
    ├── Cargo.toml
    ├── common/{mod,pool_builder,mint_builder,swap_harness,cu_budget}.rs
    ├── litesvm_unit.rs
    ├── differential_rounding.rs        # MILESTONE-1 GATE
    ├── trust_boundary.rs
    ├── token2022_filter.rs
    ├── surfpool_integration.rs
    ├── surfpool_cheatcodes.rs
    ├── historical_replay.rs            # Yellowstone / Old Faithful
    ├── fixtures/{snapshots/README.md,cu_baselines.json}
    ├── programs/swap_harness/src/lib.rs
    ├── scripts/run_surfpool.sh
    └── README.md
```

---

## 4. Cross-module interface contracts

The thirteen producer→consumer boundaries that hold the system together. A change to either side of a boundary is a breaking change to the contract.

| Producer | Consumer | Contract (type crossing the boundary) |
|---|---|---|
| **scaffold (crates/arb-config)** | onchain, detection, txbuilder, signer (ALL modules) | WAVE1_DEX_ALLOWLIST + is_allowlisted_swap_program + limits constants (MAX_TX_ACCOUNT_LOCKS=128, TX_SIZE_LIMIT_BYTES=1232, MAX_COMPUTE_UNIT_LIMIT=1_400_000). Single source of truth for program IDs and hard limits; onchain/allowlist.rs is aut…<br>`const [Pubkey;3], const fn(&Pubkey)->bool, const usize/u32/u64` |
| **scaffold (crates/arb-types)** | ALL bot + onchain modules | ArbError (repr u32, 6000-base anchor-style codes) + DexKind {RaydiumCpmm, OrcaWhirlpool, PumpSwapAmm} shared error/venue taxonomy.<br>`#[repr(u32)] enum ArbError, enum DexKind` |
| **detection (PoolStateCache / DetectionHandle)** | sizing/profit-gate (one-way; detection never depends on sizing) | DetectionHandle.signals broadcast(DetectionSignal::EdgeUpdated) + snapshot_pool(pool)->PriceView. Cache dedup keyed by pool pubkey, accept by (slot, write_version) intra-session; on reconnect prefer higher slot unconditionally (write_versio…<br>`broadcast::Receiver<DetectionSignal>, EdgeUpdate{pair,pools:Vec<PoolQuote>,best_spread_bps,max_slot}, PriceView, SessionStamp` |
| **detection (pool-cache resolved metas)** | txbuilder (route resolution + ALT static_set feed) | Resolved pool/vault/oracle AccountMetas per route for canonical remaining_accounts ordering and ALT membership.<br>`DecodedComponent, PoolQuote -> AccountMeta set` |
| **sizing (size_trade)** | txbuilder (build_arb_tx amounts + SetComputeUnitLimit floor) and executor/observ (sim_profit baseline) | SizedTrade{size_in, predicted_out per leg, min_out} sized at 90-95% optimum; off-chain integer math MUST mirror on-chain Floor(output)/Ceil(required-input) bit-exact. predicted_out validated against sim realized.<br>`SizedTrade, RoundTrip::realized_out(delta_in)->Result<u64>` |
| **onchain (TryArbitrage LiteSVM CPI harness)** | sizing/tests/differential.rs and tests/differential_rounding.rs (Milestone-1 gate) | Harness runs single-swap CPI and returns realized_out for (pool, amount_in, dir). Differential/property test asserts predicted_out == realized_out per-venue, both directions, both DEX, incl Token-2022 fee path. THIS GATES MILESTONE 1.<br>`fn(pool, amount_in:u64, dir:SwapDir)->u64 realized_out; quote_out(venue,dir,reserve_in,reserve_out,fee_bps,amount_in)->u64` |
| **onchain (process_instruction + canonical remaining_accounts convention)** | txbuilder (instruction layout) + signer (TxShapeValidator) | Strict remaining_accounts ordering for swap1/swap2 + terminal profit_assert; trust boundary requires (a) swap-CPI target allowlisted, (b) balance-read token-accounts owned by bot authority. signer/txbuilder mirror DEX allowlist + dest=own-A…<br>`VersionedMessage shape, TryArbitrageData layout (min_profit:u64), AccountMeta ordering` |
| **txbuilder (build_arb_tx)** | signer (sign_arb_tx) then executor (land) | BuiltTx = v0 VersionedTransaction with pre-warmed ALT (never extend-then-use same slot), ComputeBudget ixs, WSOL wrap/sync/close, tip transfer INSIDE atomic tx. Signer validates shape + caps before signing; executor injects/confirms tip + j…<br>`BuiltTx{VersionedTransaction, AddressLookupTableAccount[]}, ArbTxPlan, ComputeBudgetParams` |
| **signer (SignerHandle/SignerSidecar)** | executor (landing loop pre-sign) | SignerHandle.signing_enabled() checked before every sign; sign_arb_tx(msg, TxShapeClaim) enforces synchronous PreSignCaps (count + cumulative lamport-out) + TxShapeValidator (allowlist, dest=own-ATA, max-lamport-out, tip). Kill-switch flag…<br>`#[async_trait] SignerHandle, ArbSignContext{loaded_addresses, expected_lamport_out, tip_lamports, route_pool_pubkeys}, TxShapeClaim, Signatu…` |
| **executor (ExecutorMetrics / LandingOutcome)** | observability (PnlLedger, HealthEvaluator) + signer (KillSwitchSupervisor) | Landing outcome feeds RevertCause + burned_lamports + confirmation rank + submit-latency spans. Health evaluator computes revert-rate (>30%=infra bug) + burn-rate (lamports/min on losers) -> KillSwitchSignal flips signing-enabled.<br>`LandingOutcome, RevertCause, TxOutcome, HealthSnapshot, KillSwitchSignal` |
| **observability (CostModel::gate + HealthEvaluator)** | signer (synchronous pre-sign cost-gate + kill-switch) [integration seam observ-14] | CostModel::gate(CostInputs)->CostGateDecision called synchronously pre-sign (probabilistic E[net] incl p_land and loser-burn); HealthEvaluator.evaluate-> KillSwitchSignal owned-by-signer flag. observ provides gate+signal; signer owns the fl…<br>`CostInputs, CostGateDecision, KillSwitchSignal` |
| **executor (TipOracle / JitoClient)** | txbuilder (tip module) + landing loop | getTipAccounts resolved at runtime (never hardcoded); tip_floor/tip_stream sizing capped as fraction of simulated profit; tip transfer placed INSIDE atomic arb tx so fail => tip unpaid.<br>`[Pubkey;8] tip accounts, TipDecision, BundleId` |
| **sizing/math (transfer_fee_forward/inverse)** | onchain/token2022 + txbuilder/token2022 + detection/decode (live epoch fee) | Token-2022 forward vs inverse transfer-fee math is non-symmetric (floor division, <=1 unit diff); profit-check from ACTUAL balance delta not instruction amount; fee read live per-epoch (getEpochFee), never cached cross-epoch.<br>`TransferFeeConfig, transfer_fee_forward/inverse(cfg, amount)->u64` |

**Data flow:** detection (Geyser → idempotent cache → token-pair graph) emits a `DetectionSignal`/`EdgeUpdate` → sizing produces a `SizedTrade` (90–95 % of optimum, bit-exact mirror) → tx-builder assembles a v0 tx `[ComputeBudget, WSOL-wrap, swap1, swap2, profit_assert, jito_tip]` on a pre-warmed ALT → signer validates shape + synchronous caps and signs → executor lands via Jito bundle (tip inside) with Helius-Sender/SWQoS fallback → landing outcome feeds observability (revert-rate, burn-rate, latency, PnL) and the kill-switch.

---

## 5. Module designs

Each module lists its purpose, file map, the public interfaces other modules call, key data structures, dependencies, its tasks (with acceptance criteria), and open questions. Tasks are referenced by ID throughout §6–§11.

### 5.1 `scaffold` — Workspace, shared crates, toolchain, infra/config, supply-chain

**Directory:** `infra/ + workspace root`

**Purpose.** Establish the buildable foundation for the entire atomic-arbitrage system: the monorepo layout (onchain/ bot/ infra/ tests/), a pinned cargo workspace with reproducible toolchain, the on-chain program-id allowlist config (with prop-AMM truncated entries flagged unverified), the data-source provider config implementing the Chainstack/Jito ShredStream/Helius Sender cost ladder, the secrets/key layout (hot key chmod 600, never in git), CI with lockfile + integrity-hash enforcement and a verifiable-build pipeline, and the shared `arb-config` crate that every other module reads program ids / provider endpoints / safety caps from. This module owns no trading logic; it owns the contract that makes…

<details><summary><b>File map</b> (39 files)</summary>

- `Cargo.toml` — Root cargo workspace manifest.
- `Cargo.lock` — Committed lockfile pinning the full transitive dependency graph (binary AND library workspace).
- `rust-toolchain.toml` — Pins the exact Rust toolchain so every dev + CI + the solana-verify reproducible build uses identical rustc.
- `rustfmt.toml` — Shared formatting config so CI fmt-check is deterministic.
- `clippy.toml` — Lint configuration;
- `.gitignore` — Prevents secrets and build artifacts from entering git — directly enforces the §11 invariant that hot keypairs are never committed.
- `.env.example` — Template enumerating every env var the bot/signer read, with placeholder (non-secret) values.
- `README.md` — Top-level repo orientation: layout, quickstart, toolchain bootstrap, security warning.
- `Makefile` — Single entrypoint for common dev/CI tasks so onboarding and CI share commands.
- `crates/arb-config/Cargo.toml` — Manifest for the shared, no_std-capable config crate consumed by BOTH onchain program and offchain bot.
- `crates/arb-config/src/lib.rs` — Public API of the config crate: re-exports program ids, hard limits, provider config loader.
- `crates/arb-config/src/program_ids.rs` — no_std const table of all pinned program ids.
- `crates/arb-config/src/limits.rs` — no_std compile-time constants for the protocol hard limits — the binding ceilings every other module must respect.
- `crates/arb-config/src/providers.rs` — std-side typed model of the data-source/landing ladder loaded from infra/config/providers.toml.
- `crates/arb-config/src/secrets.rs` — std-side loader + validator for the hot keypair and kill-switch, enforcing the key-mgmt invariant at load time.
- `crates/arb-config/src/loader.rs` — Parses infra/config/*.toml into the typed structs, and exposes a self-validation routine used by `make config-check` and CI.
- `crates/arb-types/Cargo.toml` — Manifest for shared lightweight types/error enums used across onchain+offchain (no_std-friendly).
- `crates/arb-types/src/lib.rs` — Cross-module shared types so onchain and bot agree on wire/ABI shapes without circular deps.
- `crates/arb-math/Cargo.toml` — Placeholder manifest for the bit-exact integer-math crate (owned/implemented by the math module) so the workspace compiles from day 1.
- `onchain/arb-program/Cargo.toml` — Manifest for the native on-chain program (hot path).
- `onchain/arb-program/src/lib.rs` — Scaffold entrypoint stub so the workspace builds;
- `bot/arb-bot/Cargo.toml` — Manifest for the off-chain hot-path bot binary.
- `bot/arb-bot/src/main.rs` — Scaffold main that loads ArbConfig, checks kill-switch, prints resolved ladder tier, then exits — proves the config/secrets contract end-to-end.
- `bot/arb-signer/Cargo.toml` — Manifest for the in-process signer sidecar binary holding only the low-balance hot key.
- `bot/arb-signer/src/main.rs` — Scaffold signer that loads the chmod-600 hot key via arb_config::secrets and refuses to start if the kill-switch file is present or key perms are wrong.
- `tests/litesvm-tests/Cargo.toml` — Manifest for the LiteSVM integration-test harness crate (test substrate per §8).
- `tests/litesvm-tests/tests/smoke.rs` — A trivial green test proving the workspace + LiteSVM wiring compiles and runs in CI.
- `infra/config/program_ids.toml` — Human-editable source-of-truth program-id pin file (verified on Solscan).
- `infra/config/providers.toml` — The data-source + landing ladder config (§8).
- `infra/config/limits.toml` — Documentation-grade mirror of the compiled hard-limit constants for ops visibility (compiled values in limits.rs are authoritative).
- `infra/toolchain/versions.toml` — Pins all non-cargo tool versions for reproducible bootstrap and CI parity.
- `infra/scripts/install-toolchain.sh` — Idempotent bootstrap script consumed by `make bootstrap` that installs every pinned tool from versions.toml.
- `infra/scripts/verify-config.sh` — Wraps `cargo run -p arb-config --bin config-check` (or test) so CI and devs validate config consistency identically.
- `infra/scripts/gen-program-keypair.sh` — Generates the upgradeable program keypair and the throwaway hot keypair with correct perms;
- `infra/deny.toml` — cargo-deny policy: bans yanked/duplicate/known-vuln deps and enforces the supply-chain integrity gate (§11).
- `infra/audit/cargo-vet/ or integrity-hashes.lock` — Records integrity (sha256) hashes for git-pinned / non-crates.io deps to satisfy the §11 'integrity hash' requirement beyond Cargo.lock checksums.
- `secrets/README.md` — Documents the secrets directory contract;
- `.github/workflows/ci.yml` — CI pipeline enforcing build, lint, test, lockfile integrity, supply-chain audit, and config consistency on every PR.
- `.github/workflows/verifiable-build.yml` — Reproducible/verifiable build pipeline (§11 deploy posture) producing a solana-verify hash for the on-chain program.

</details>

**Public interfaces (10):**

```rust
// arb_config::program_ids::WAVE1_DEX_ALLOWLIST — The canonical Wave-1 swap-program allowlist (Raydium CPMM, Orca Whirlpool, PumpSwap AMM).
pub const WAVE1_DEX_ALLOWLIST: [solana_program::pubkey::Pubkey; 3]
// arb_config::program_ids::is_allowlisted_swap_program — const-evaluable membership test against WAVE1_DEX_ALLOWLIST.
pub const fn is_allowlisted_swap_program(program_id: &Pubkey) -> bool
// arb_config::limits — Compile-time protocol hard limits consumed by the tx-builder (account-lock + byte budgeting), the signer (pre-sign shape caps), and CU-limit computati…
pub const MAX_TX_ACCOUNT_LOCKS: usize = 128; pub const MAX_LOADED_ACCOUNTS: usize = 256; pub const TX_SIZE_LIMIT_BYTES: usize = 1232; pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000; pub const BASE_FEE_LAMPORTS_PER_SIG: u64 = 5_000; pub const CU_LIMIT_SIM_MARGIN_BPS: u32 = 1_000;
// arb_config::loader::load — Loads and parses infra/config/{program_ids,providers,limits}.toml into the typed ArbConfig.
pub fn load(config_dir: impl AsRef<std::path::Path>) -> Result<ArbConfig, ConfigError>
// arb_config::loader::validate — Self-consistency gate: asserts the toml program ids equal the compiled const table, no unverified prop-AMM appears in any allowlist, the active ladder…
pub fn validate(cfg: &ArbConfig) -> Result<(), ConfigError>
// arb_config::secrets::load_hot_keypair — The ONLY sanctioned hot-key loader.
pub fn load_hot_keypair(path: &std::path::Path) -> Result<solana_sdk::signer::keypair::Keypair, SecretError>
// arb_config::secrets::kill_switch_engaged — Returns true if the kill-switch file is present/set.
pub fn kill_switch_engaged(path: &std::path::Path) -> bool
// arb_config::providers::ArbConfig::active_landing — Resolves the landing config (Jito primary with tip-inside-tx, Helius Sender fallback) for the executor module.
pub fn active_landing(&self) -> &LandingConfig
// arb_types::ArbError — Stable error-code enum shared so the onchain program's returned codes and the bot's revert-reason decoding agree.
#[repr(u32)] pub enum ArbError { Unprofitable = 6000, UnauthorizedProgram = 6001, UnauthorizedTokenAccountOwner = 6002, ForbiddenTokenExtension = 6003, /* ... */ }
// arb_types::DexKind — Venue discriminant shared across detection/sizing/builder.
pub enum DexKind { RaydiumCpmm, OrcaWhirlpool, PumpSwapAmm }
```

**Key data structures:**

- **`ProgramIdEntry / ProgramIdStatus`** — `pub enum ProgramIdStatus { Verified { solscan_checked_on: &'static str }, DeferredWave2, Unverified } ; pub struct ProgramIdEntry { pub name: &'static str, pub…` · Invariant: only Verified entries whose name is a Wave-1 venue may appear in WAVE1_DEX_ALLOWLIST.
- **`ArbConfig`** — `pub struct ArbConfig { pub cluster: Cluster, pub program_ids: ProgramIdTable, pub data_source: DataSourceConfig, pub landing: LandingConfig, pub limits: LimitsV…` · Top-level loaded config.
- **`DataSourceConfig`** — `pub struct DataSourceConfig { pub active_tier: LadderTier, pub grpc: Option<GrpcEndpoint>, pub fallback_wss: Option<Url>, pub json_rpc: Url, pub shredstream: Sh…` · Default active_tier = FirstProfit (Chainstack Growth + Yellowstone gRPC add-on per §8).
- **`GrpcEndpoint`** — `pub struct GrpcEndpoint { pub url: Url, pub token_env: String, pub commitment: Commitment /* processed */, pub max_streams: u8 }` · token resolved from env (token_env names the var), never stored in toml.
- **`ShredStreamConfig`** — `pub struct ShredStreamConfig { pub proxy_addr: SocketAddr, pub enabled: bool }` · Free across all tiers;
- **`LandingConfig`** — `pub struct LandingConfig { pub jito: JitoConfig, pub helius_sender: SenderConfig } ; pub struct JitoConfig { pub block_engine_url: Url, pub auth_uuid_env: Strin…` · Hard invariant enforced by validate(): jito.tip_inside_tx == true (tip lives inside the atomic tx so failure => tip not paid).
- **`SecretsLayout (contract, not a struct)`** — `secrets/ dir: hot-keypair.json (0o600, gitignored), kill_switch (presence => halt), README.md + .gitkeep tracked. Upgrade/treasury authority NOT here (KMS+Squad…` · Enforced by .gitignore + secrets::load_hot_keypair perm check.
- **`Cluster`** — `pub enum Cluster { MainnetBeta, SurfpoolFork, LiteSvm }` · Drives which config/endpoints are valid;

**External crates:** `solana-program =2.1.0 (Agave 2.x; Pubkey + onchain, §8)`, `solana-sdk =2.1.0 (offchain signing/types, §8)`, `solana-client =2.1.0 (RPC for bot, §8)`, `solana-nostd-entrypoint =0.6.0 (hot-path onchain entrypoint, §8)`, `bytemuck =1.18.0 (zero-copy decode scaffold, §8)`, `uint =0.9.5 + spl-math (U128/U256 for math crate, §8)`, `yellowstone-grpc-client / yellowstone-grpc-proto (v13.x +solana.4.0.0 tracking release, §8)`, `jito-sdk-rust (git rev-pinned + integrity hash, §8)`, `spl-token =6.0.0, spl-token-2022 =5.0.0, spl-associated-token-account =4.0.0`, `tokio =1.40.0, serde =1.0.210, serde_json, toml =0.8.19, thiserror, tracing, tracing-subscriber, prost, tonic`, `litesvm (pinned, dev/test substrate, §8)`, `tooling-only (not workspace deps): solana-cli/Agave 2.1.0, anchor 0.30.1 (prototyping ONLY), surfpool, solana-verify, cargo-audit, cargo-deny`

**Grounded in `plan.md`:** §1-3 (architecture the workspace mirrors); §8 (tech stack, crate pins, data-source/landing ladder, testing substrate); §11 Fase 0 (monorepo, toolchain, config pinning, secrets, lockfile+integrity, study repos); §13 (study repos, malware-cluster exclusion, verifiable build)

**Tasks (12):**

- **`scaffold-1` Initialize git repo, monorepo skeleton, and .gitignore secrets guard** · Fase 0 · 0.5d · deps: —
  - Create the onchain/ bot/ infra/ tests/ crates/ directory tree per §11 Fase 0. Add .gitignore that excludes /target, *.so, /secrets/** (except README+.gitkeep), and all keypair/.env patterns BEFORE any key material exists. Add README.md with the layout map and the §13 malware/security warning. Add secrets/README.md + .g…
  - **Done when:** Directory tree onchain/ bot/ infra/{config,scripts,toolchain,audit} tests/ crates/ exists · Creating a dummy secrets/hot-keypair.json and a .env => `git status --porcelain` lists neither · README contains the explicit 'never fork Solana-Arbitrage-Bot malware cluster' warning and links the §13 study repos
- **`scaffold-2` Pin toolchain: rust-toolchain.toml + infra/toolchain/versions.toml + bootstrap script** · Fase 0 · 1d · deps: `scaffold-1`
  - Pin rustc via rust-toolchain.toml (1.79.0) and ALL non-cargo tools in infra/toolchain/versions.toml (Agave/solana-cli 2.1.0, platform-tools sbf, Anchor 0.30.1 tooling-only, litesvm, surfpool, solana-verify, cargo-audit, cargo-deny). Write infra/scripts/install-toolchain.sh that installs exactly those versions and verif…
  - **Done when:** `make bootstrap` installs and version-verifies rust, solana-cli (Agave), anchor, litesvm deps, surfpool, solana-verify; mismatched version => non-zero exit · versions.toml comment explicitly states Anchor is tooling/prototyping-only and the hot path is native solana-program · `solana --version` and `anchor --version` match versions.toml after bootstrap
- **`scaffold-3` Create cargo workspace with centralized pinned [workspace.dependencies] and committed lockfile** · Fase 0 · 1.5d · deps: `scaffold-2`
  - Author root Cargo.toml with resolver=2, all members, and exact '=' version pins for every crate from §8 under [workspace.dependencies]. Add release profile (overflow-checks=true, lto, panic=abort for onchain). Create stub member manifests + lib.rs/main.rs for arb-config, arb-types, arb-math, arb-program, arb-bot, arb-s…
  - **Done when:** `cargo build --workspace --locked` passes; `cargo build-sbf` on onchain/arb-program passes · Every version in [workspace.dependencies] uses '=' exact pin matching §8 (solana 2.1.0, yellowstone 4.0.0, bytemuck 1.18.0, uint 0.9.5, web3-equivalent not present) · Cargo.lock is committed and `git diff --exit-code Cargo.lock` is clean after a fresh build
- **`scaffold-4` Implement arb-config no_std core: program_ids + limits constants** · Fase 0 · 1d · deps: `scaffold-3`
  - Implement program_ids.rs (Wave-1 verified ids Raydium CPMM / Orca Whirlpool / PumpSwap AMM, plus token/ata/compute-budget/alt/system; Raydium AMM v4 as DeferredWave2; prop-AMM truncated as Unverified) with WAVE1_DEX_ALLOWLIST + const is_allowlisted_swap_program. Implement limits.rs (128/256/1232/1.4M/5000/10%). Make th…
  - **Done when:** `cargo test -p arb-config --no-default-features` passes the allowlist-purity test · WAVE1_DEX_ALLOWLIST == [CPMMoo8L..., whirLbMi..., pAMMBay6...] verbatim; is_allowlisted_swap_program(RAYDIUM_AMM_V4)==false and false for every prop-AMM · onchain/arb-program builds via `cargo build-sbf` while importing the allowlist (proves no_std linkage) · limits.rs exports MAX_TX_ACCOUNT_LOCKS=128 (not 256), TX_SIZE_LIMIT_BYTES=1232, MAX_COMPUTE_UNIT_LIMIT=1_400_000
- **`scaffold-5` Implement arb-config std side: providers/landing ladder, secrets loader, loader+validate** · Fase 0 · 1.5d · deps: `scaffold-4`, `scaffold-6`
  - Implement providers.rs (LadderTier, DataSourceConfig defaulting to FirstProfit/Chainstack, GrpcEndpoint commitment=processed, ShredStreamConfig, LandingConfig with jito.tip_inside_tx + Helius Sender fallback). Implement secrets.rs (load_hot_keypair with 0o600 enforcement, kill_switch_engaged). Implement loader.rs load(…
  - **Done when:** validate() returns Err if a config sets landing.jito.tip_inside_tx=false · validate() returns Err if any unverified prop-AMM id appears in an allowlist or if the active tier lacks its required gRPC endpoint · load_hot_keypair on a 0o644 file returns SecretError on unix; on a 0o600 file returns the Keypair · Default loaded DataSourceConfig.active_tier == FirstProfit (Chainstack) and a [skipped] helius_business note is parsed/ignored
- **`scaffold-6` Author infra/config TOMLs: program_ids, providers, limits** · Fase 0 · 0.5d · deps: `scaffold-1`
  - Write program_ids.toml (verified Wave-1 with verified_on/solscan placeholders, wave2 raydium_amm_v4 deferred, unverified_prop_amm humidifi/tessera/goonfi). Write providers.toml encoding the full §8 ladder (build_proof ~$0-40, first_profit Chainstack ~$98-198 DEFAULT, niche_firehose, competitive) + landing (jito tip_ins…
  - **Done when:** providers.toml active_tier="first_profit" with chainstack growth+yellowstone-gRPC add-on, and a [skipped.helius_business] entry citing $499 · program_ids.toml lists exactly the 3 Wave-1 ids as verified and the prop-AMMs as status=unverified · limits.toml numeric values equal limits.rs consts (asserted by config-check)
- **`scaffold-7` Config-consistency tooling: verify-config.sh + program-id Solscan cross-check** · Fase 0 · 0.5d · deps: `scaffold-5`, `scaffold-6`
  - Implement a config-check entry (cargo test or small bin) and infra/scripts/verify-config.sh that runs loader::validate AND diffs program_ids.toml against the compiled program_ids.rs table, failing if any divergence, any unverified prop-AMM in an allowlist, or tip_inside_tx!=true. Document the manual Solscan verificatio…
  - **Done when:** Flipping one program id in program_ids.toml makes `make config-check` exit non-zero · Adding a prop-AMM id into WAVE1_DEX_ALLOWLIST makes config-check fail · Setting tip_inside_tx=false makes config-check fail
- **`scaffold-8` Key/program-keypair generation script + secrets contract enforcement** · Fase 0 · 0.5d · deps: `scaffold-5`
  - Implement gen-program-keypair.sh that generates the upgradeable program keypair (writes program id into declare_id! + program_ids.toml) and a throwaway low-balance hot keypair, both chmod 600 under /secrets. Implement the arb-signer scaffold main that loads the hot key ONLY via arb_config::secrets and refuses to start…
  - **Done when:** Generated keypairs are mode 0o600 and not tracked by git · arb-signer scaffold exits non-zero if kill_switch file present or key perms != 0o600 · Treasury/upgrade authority is NOT generated into /secrets (documented as KMS+Squads)
- **`scaffold-9` Supply-chain integrity: deny.toml, integrity-hashes, cargo-audit/deny wiring** · Fase 0 · 1d · deps: `scaffold-3`
  - Author infra/deny.toml (deny vulns/yanked, pin sources to crates.io + explicit git revs, license allowlist, ban risky crates incl. vulnerable web3 ranges). Add integrity-hashes.lock + verify-integrity.sh recording sha256 for git-pinned deps (jito-sdk-rust, spl-math, yellowstone tracking release). Add a pre-commit secre…
  - **Done when:** cargo-deny check passes; sources limited to crates.io + listed git revs · Altering a git-pinned dep rev without updating integrity-hashes.lock makes verify-integrity.sh fail · cargo-audit reports zero known-vuln advisories on the committed lockfile
- **`scaffold-10` CI pipeline: build/lint/test/lockfile/audit/config gates** · Fase 0 · 1d · deps: `scaffold-7`, `scaffold-9`, `scaffold-11`
  - Author .github/workflows/ci.yml running on PR: bootstrap (cached pinned toolchain), fmt --check, clippy -D warnings (arithmetic_side_effects on arb-math), build workspace + build-sbf, lockfile integrity (cargo build --locked + git diff --exit-code Cargo.lock), make audit, make config-check, and the LiteSVM smoke test.…
  - **Done when:** PR introducing an unpinned/updated dep that changes Cargo.lock fails the lockfile job · PR with a clippy warning fails; PR with a config inconsistency fails config-check · CI uses exactly the versions.toml-pinned solana-cli/rust
- **`scaffold-11` LiteSVM + Surfpool test substrate wiring and smoke test** · Fase 0 · 1d · deps: `scaffold-4`
  - Wire tests/litesvm-tests to load the built arb-program .so into LiteSVM and run a smoke test asserting the allowlist consts resolve and a deliberately-unprofitable stub path returns FailedTransactionMetadata (placeholder for the real revert proof owned by the onchain/test modules). Add `make test-surfpool` invoking a S…
  - **Done when:** LiteSVM smoke test loads the program and passes in CI · Surfpool fork successfully clones the Raydium CPMM and Orca Whirlpool pools by pubkey (manual/local run documented) · Test harness reads pool/program ids from arb-config (no hardcoded ids in tests)
- **`scaffold-12` Verifiable/reproducible build pipeline (solana-verify) + Squads deploy doc** · Fase 1 · 1d · deps: `scaffold-10`
  - Author .github/workflows/verifiable-build.yml that on tag runs `solana-verify build` in a pinned container, emits the program hash artifact, and documents the manual Squads-multisig upgrade-authority deploy handoff (CI produces+verifies the artifact only; deploy is multisig-gated). Forward hook for Fase 1 deploy postur…
  - **Done when:** `solana-verify build` produces a stable hash across two runs on the pinned container · Doc states upgrade authority = Squads multisig and that CI never holds it · Verifiable-build workflow uses the same versions.toml pins as ci.yml

<details><summary><b>Open questions</b> (7)</summary>

- Exact pinned versions: §8 gives Agave 2.x/4.x as a range and yellowstone as 'v13.x +solana.4.0.0' — needs a single concrete pin decision (I provisionally pinned Agave 2.1.0 / yellowstone 4.0.0 tracking release; confirm against the on-chain runtime version targeted at deploy).
- Verified on-chain hash/program id for PumpSwap AMM and exact verified-on dates for all Wave-1 ids must be filled by an operator running the Solscan verification step (scaffold provides the table + CI cross-check, not the verification itself).
- Truncated prop-AMM full ids (HumidiFi/Tessera/GoonFi) are intentionally left unverified — open whether to store the known partial prefixes at all or omit entirely until verified.
- Whether arb-signer is a separate process/binary communicating over a local socket vs an in-process module — scaffold provisions a separate binary (safer isolation per key-mgmt invariant); the signer module may collapse it. Affects whether secrets crate exposes an IPC surface.
- Integrity-hash mechanism: cargo-vet vs a hand-maintained integrity-hashes.lock vs relying solely on Cargo.lock checksums + cargo-deny source pinning. §11 says 'integrity hash' without specifying tooling.
- CI runner OS for build-sbf and solana-verify (reproducible builds typically require a pinned Docker image) — needs a chosen base image digest.
- Anchor 0.30.1 vs newer for tooling — pin must track whatever IDL/prototyping work the team actually uses; it is tooling-only so low-risk to bump.

</details>

---

### 5.2 `onchain` — On-chain program — native-Rust `TryArbitrage` (2 swap CPI + terminal profit-assert)

**Directory:** `onchain/`

**Purpose.** A native Rust (solana-program, NOT Anchor) Solana program exposing a single hot-path instruction `TryArbitrage` that executes a 2-leg atomic round-trip arbitrage in one transaction: snapshot the bot's base-asset ATA balance, perform swap CPI leg A, read the actual balance delta of the intermediate ATA (Token-2022-aware), feed it as leg-B input, perform swap CPI leg B, then run a terminal profit-assert `require(post >= pre + min_profit)` that returns `Err(ArbitrageError::Unprofitable)` so the runtime reverts ALL state when unprofitable. The program is the ONLY real safety net (must hold under skipPreflight=true). It enforces the trust boundary on untrusted `remaining_accounts`: every swap-CPI…

<details><summary><b>File map</b> (21 files)</summary>

- `onchain/Cargo.toml` — Crate manifest for the native program.
- `onchain/src/lib.rs` — Crate root.
- `onchain/src/entrypoint.rs` — BPF entrypoint.
- `onchain/src/instruction.rs` — Instruction discriminator + instruction-data layout.
- `onchain/src/processor.rs` — Hot-path orchestration of TryArbitrage.
- `onchain/src/error.rs` — Custom error enum mapped to ProgramError::Custom(u32).
- `onchain/src/allowlist.rs` — Compile-time pinned DEX program ids (the trust anchor).
- `onchain/src/trust.rs` — Trust-boundary verification on untrusted remaining_accounts.
- `onchain/src/state.rs` — Zero-copy token-account balance reads.
- `onchain/src/token2022.rs` — On-chain Token-2022 extension filter (defense-in-depth mirror of off-chain routing filter).
- `onchain/src/adapters/mod.rs` — Per-DEX swap adapter dispatch.
- `onchain/src/adapters/raydium_cpmm.rs` — Raydium CPMM swap adapter.
- `onchain/src/adapters/orca_whirlpool.rs` — Orca Whirlpool swap_v2 adapter.
- `onchain/src/adapters/pumpswap.rs` — PumpSwap AMM adapter (Fase 2).
- `onchain/src/constants.rs` — Program constants: AMOUNT_OFFSET=64, ACCOUNT_OWNER_OFFSET=32, SPL token account base size 165, expected fixed-account count, and CU/cost notes.
- `onchain/tests/litesvm_revert.rs` — LiteSVM unit tests: (a) no-arb config -> TryArbitrage returns Err(Unprofitable), assert FailedTransactionMetadata + custom code 1, assert zero net token movement;
- `onchain/tests/token2022_filter.rs` — LiteSVM tests for token2022::vet_mint: craft mints with each forbidden extension (TransferHook non-null, NonTransferable, DefaultAccountState=frozen, MemoTransfer, Confid…
- `onchain/tests/rounding_mirror_fuzz.rs` — Property/fuzz gate (Milestone-1 gating, §7/§12).
- `onchain/tests/common/mod.rs` — Shared test fixtures: load Raydium CPMM, Orca Whirlpool, PumpSwap programs into LiteSVM;
- `onchain/build-verifiable.sh` — Reproducible/verifiable build script using solana-verify (docker-pinned toolchain) to produce deterministic bytecode matching on-chain;
- `onchain/README.md` — Module doc: instruction-data layout, remaining_accounts ordering convention per venue, trust-boundary guarantees, error code table, CU budget notes, deploy/upgrade runboo…

</details>

**Public interfaces (10):**

```rust
// process_instruction — Program entrypoint dispatch.
pub fn process_instruction(program_id: &Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult
// TryArbitrageData::unpack — Manual little-endian parse of instruction data into {min_profit:u64, leg_a:LegDescriptor, leg_b:LegDescriptor}.
pub fn unpack(data: &[u8]) -> Result<TryArbitrageData, ArbitrageError>
// trust::verify_swap_program — Asserts the CPI target program key equals the compile-time-pinned allowlist id for `dex` and that it is executable;
pub fn verify_swap_program(target_program: &AccountInfo, dex: Dex) -> Result<(), ArbitrageError>
// trust::verify_balance_account — Zero-copy asserts the token account's owner field (offset 32) == bot_authority and the account is owned by an allowlisted token program;
pub fn verify_balance_account(token_account: &AccountInfo, bot_authority: &Pubkey) -> Result<(), ArbitrageError>
// state::read_token_amount — Zero-copy read of SPL/Token-2022 token-account `amount` at offset 64 (8 LE bytes) without full unpack.
pub fn read_token_amount(token_account: &AccountInfo) -> Result<u64, ArbitrageError>
// token2022::vet_mint — Unpacks PodStateWithExtensions<PodMint>;
pub fn vet_mint(mint: &AccountInfo) -> Result<(), ArbitrageError>
// adapters::dispatch — Routes to the per-venue SwapAdapter, which verifies program id, vets mints, and performs the swap CPI via invoke.
pub fn dispatch(dex: Dex, accounts: &[AccountInfo], amount_in: u64, min_out: u64, a_to_b: bool) -> Result<(), ArbitrageError>
// SwapAdapter (trait) — Per-DEX adapter contract implemented by raydium_cpmm, orca_whirlpool, pumpswap.
pub trait SwapAdapter { const DEX: Dex; fn swap(accounts: &[AccountInfo], amount_in: u64, min_out: u64, a_to_b: bool) -> Result<(), ArbitrageError>; }
// allowlist::is_allowlisted_dex — Returns true iff program_id is one of the compile-time-pinned Wave-1 DEX ids (Raydium CPMM, Orca Whirlpool, PumpSwap).
pub fn is_allowlisted_dex(program_id: &Pubkey) -> bool
// ArbitrageError — Stable custom error codes surfaced as ProgramError::Custom(u32).
#[repr(u32)] pub enum ArbitrageError { NoArbitrage=0, Unprofitable=1, SlippageExceeded=2, InvalidRoute=3, InvalidAccountsList=4, UnauthorizedSwapProgram=5, BalanceAccountNotBotOwned=6, ForbiddenMintExtension=7, MathOverflow=8, MissingAccount=9, MintParseFailed=10 }
```

**Key data structures:**

- **`TryArbitrageData`** — `struct { min_profit: u64, leg_a: LegDescriptor, leg_b: LegDescriptor }` · Only min_profit is trusted from client.
- **`LegDescriptor`** — `struct { dex: Dex(u8), accounts_offset: u8, accounts_len: u8, a_to_b: bool }` · Tells the processor which remaining_accounts slice belongs to this leg and which venue adapter to use.
- **`Dex`** — `#[repr(u8)] enum { RaydiumCpmm=0, OrcaWhirlpool=1, PumpSwap=2 }` · Maps to a pinned allowlist program id.
- **`ArbitrageError`** — `#[repr(u32)] enum (see interfaces)` · Append-only numeric codes.
- **`Fixed account order (TryArbitrage)`** — `[0]=bot_authority(signer,writable), [1]=base_ata(writable), [2]=intermediate_ata(writable), [3]=spl_token_program, [4]=token_2022_program, [5..]=remaining_accou…` · bot_authority signs in the OUTER tx;
- **`Venue account-ordering convention`** — `Per-adapter const slice layout (see adapters/* files): RaydiumCpmm 13 accounts, OrcaWhirlpool swap_v2 ~15 accounts incl 3 tick arrays+oracle, PumpSwap ~12 accou…` · Single source of truth shared between adapter (parse) and client tx-builder (emit).

**External crates:** `solana-program (=Agave-matched version, e.g. =1.18.x or =2.x pinned)`, `spl-token`, `spl-token-2022 (PodStateWithExtensions, PodMint, ExtensionType)`, `spl-associated-token-account`, `bytemuck (zero-copy, derive)`, `thiserror or manual error impl`, `(dev) litesvm`, `(dev) proptest`, `(dev) solana-sdk for test instruction building`

**Grounded in `plan.md`:** §1 (scope: 2-pool CPMM Raydium CPMM + Orca Whirlpool, inventory not flash loan); §2 (atomicity = runtime property; Err reverts all; tip inside tx); §6 (program design, trust boundary, CPI depth/cost, hard limits, deploy posture, instruction-data layout); §7 (integer math, Floor/Ceil rounding, both-direction fuzz gate); §9 (Token-2022 extension hard-reject matrix, balance-delta profit, WSOL, fee asymmetry); §11 Fase 0/1/2 (build order); §12 (Definition of Done, cross-phase quality gates); §13 (reference repos: buffalojoec/arb-program, 0xNineteen rust-macros-arbitrage, raydium-cpi-example, orca whirlpool-cpi-sample)

**Tasks (14):**

- **`onchain-1` Crate scaffold + entrypoint + verifiable-build setup** · Fase 0 · 2d · deps: —
  - Create onchain/ cargo crate (cdylib+lib, no-entrypoint feature), pin solana-program/spl-token/spl-token-2022/bytemuck to Agave-matched versions, set release profile (overflow-checks, lto, codegen-units=1, panic=abort) for determinism. Build the buffalojoec/arb-program skeleton locally as reference and confirm it compil…
  - **Done when:** cargo build-sbf produces a .so; solana-verify get-executable-hash emits a stable hash on two runs · Crate exposes lib with feature no-entrypoint so tests can import it · README stub documents deploy posture (upgrade authority = Squads multisig) + solana-verify publish step
- **`onchain-2` Error enum + instruction-data layout + Dex/LegDescriptor** · Fase 1 · 2d · deps: `onchain-1`
  - Implement error.rs (stable u32 codes), instruction.rs manual LE parse for TryArbitrageData{min_profit, leg_a, leg_b} and Dex/LegDescriptor with strict length + slice-bounds validation. Define the fixed-account order. Establish min_profit-covers-all-costs semantics in README (costs not trusted from client).
  - **Done when:** unpack rejects short/trailing/over-long data with InvalidRoute · LegDescriptor slice bounds beyond remaining_accounts -> InvalidAccountsList · Error codes match the documented table exactly (asserted in a unit test) · Byte layout documented and matched by a test vector the off-chain builder can reuse
- **`onchain-3` Pinned allowlist + trust-boundary verification** · Fase 1 · 2d · deps: `onchain-1`, `onchain-2`
  - Implement allowlist.rs with compile-time-pinned Wave-1 program ids (Raydium CPMM, Orca Whirlpool, PumpSwap) + token programs/WSOL, verified on Solscan (§12). Implement trust.rs verify_swap_program (target==pinned id, executable) and verify_balance_account (owner@offset32==bot_authority, owned by allowlisted token progr…
  - **Done when:** verify_swap_program rejects non-allowlisted/non-executable program -> UnauthorizedSwapProgram(5) · verify_balance_account rejects ATA whose owner != bot_authority -> BalanceAccountNotBotOwned(6) · Pinned ids byte-match the spec ids; on-chain verification documented as the only fee safety net (holds under skipPreflight)
- **`onchain-4` Zero-copy balance read (state.rs)** · Fase 1 · 1d · deps: `onchain-1`
  - Implement read_token_amount (offset 64, 8 LE) and read_owner (offset 32) without full unpack, valid for SPL + Token-2022 base layout. This is the balance-delta primitive for pre/post snapshots.
  - **Done when:** read_token_amount returns exact amount for both SPL and Token-2022 (incl fee-config) accounts · Reads are bounds-checked (short data -> MissingAccount/MintParseFailed, no panic)
- **`onchain-5` Token-2022 extension filter (token2022.rs)** · Fase 1 · 2d · deps: `onchain-1`, `onchain-2`
  - Implement vet_mint via PodStateWithExtensions<PodMint>; HARD-REJECT the forbidden extension set incl MintCloseAuthority; allow plain SPL, fee-only Token-2022, and TransferHook(program_id=None). Document the null-hook nuance.
  - **Done when:** Each forbidden extension (TransferHook non-null, NonTransferable, DefaultAccountState=frozen, MemoTransfer, ConfidentialTransfer, PermanentDelegate, MintCloseAuthority) -> ForbiddenMintExtension(7) · Plain SPL + fee-only Token-2022 + null-hook mint pass · Test asserts behavior, not just compiles
- **`onchain-6` Raydium CPMM swap adapter** · Fase 1 · 3d · deps: `onchain-3`, `onchain-4`, `onchain-5`
  - Implement adapters/raydium_cpmm.rs: documented 13-account ordering, swap_base_input discriminator/args, program-id verify, mint vetting, invoke. Add the SwapAdapter trait + dispatch in adapters/mod.rs.
  - **Done when:** Adapter performs a real Raydium CPMM swap CPI in LiteSVM and moves expected tokens · Rejects mismatched program id before invoke · Account-order layout documented and matched by tests/common builder
- **`onchain-7` Orca Whirlpool swap_v2 adapter** · Fase 1 · 3d · deps: `onchain-3`, `onchain-4`, `onchain-5`
  - Implement adapters/orca_whirlpool.rs: swap_v2 (Token-2022-aware) ~15-account ordering incl 3 tick arrays + oracle, args (amount, other_amount_threshold, sqrt_price_limit, amount_specified_is_input=true, a_to_b), sqrt_price_limit clamp, program-id verify, mint vetting, invoke.
  - **Done when:** Adapter performs a real Whirlpool swap_v2 CPI in LiteSVM and moves expected tokens both directions · sqrt_price_limit clamps prevent u64 overflow on extreme price · Tick-array accounts forwarded in correct order
- **`onchain-8` Processor: snapshot -> CPI A -> delta -> CPI B -> terminal assert** · Fase 1 · 3d · deps: `onchain-6`, `onchain-7`
  - Implement processor.rs orchestration: parse, resolve fixed accounts, verify balance accounts bot-owned, pre=read(base_ata); per leg: slice remaining_accounts, verify swap program, vet mints, mid_pre/mid_post balance-delta to compute amount_in for next leg; post=read(base_ata); require(post >= pre.checked_add(min_profit…
  - **Done when:** No-arb input -> Err(Unprofitable=1), runtime reverts all, zero net token movement (LiteSVM) · Profitable input -> success, base-asset delta == off-chain prediction · leg-B amount_in derived from actual intermediate ATA balance delta (Token-2022 fee-safe), not instruction amount · All math checked; overflow -> MathOverflow(8) · Borrows on account data dropped before each invoke
- **`onchain-9` LiteSVM unit tests: revert, success, trust-boundary, CU** · Fase 1 · 3d · deps: `onchain-8`
  - Implement tests/litesvm_revert.rs + tests/common: no-arb revert with code 1 + zero movement; profitable success + exact delta; trust-boundary negatives (UnauthorizedSwapProgram, BalanceAccountNotBotOwned); measure CU per leg + total < 1.4M. Single-source the account-order builders.
  - **Done when:** FailedTransactionMetadata asserted with custom code 1 on no-arb · Trust-boundary negatives return codes 5 and 6 · CU per leg printed; total tx CU < 1.4M asserted · Zero net token movement on revert asserted
- **`onchain-10` Rounding-mirror fuzz/property gate (per-venue, both directions) — MILESTONE-1 GATE** · Fase 1 · 4d · deps: `onchain-9`
  - Implement tests/rounding_mirror_fuzz.rs: for Raydium CPMM and Orca Whirlpool, both directions, proptest over (reserves, fees, amount_in); assert off-chain bot::math predicted_out == on-chain CPI realized_out (LiteSVM balance delta), Floor-output/Ceil-input bit-exact, incl Token-2022 fee path. This GATES Milestone 1 (§1…
  - **Done when:** predicted_out == realized_out for both venues, both directions, across wide fuzz ranges (no single-example shortcut) · Token-2022 fee-only path included in the fuzz · CI fails the build if any case diverges
- **`onchain-11` Surfpool mainnet-fork integration test (revert on real programs)** · Fase 1 · 3d · deps: `onchain-10`
  - Run TryArbitrage against real Raydium CPMM + Orca Whirlpool programs/pools cloned via Surfpool; prove revert on deliberately unprofitable input with no net token movement, and a profitable success path on a constructed dislocation. Patch oracle slot only if needed (minimal for non-oracle legs).
  - **Done when:** Cloned Raydium CPMM + Orca Whirlpool pools usable in fork · Unprofitable input reverts cleanly, zero net token movement · Account locks < 128 and tx < 1232 bytes on the assembled tx (verified by builder)
- **`onchain-12` PumpSwap AMM adapter (Fase 2 venue)** · Fase 2 · 3d · deps: `onchain-8`, `onchain-10`
  - Implement adapters/pumpswap.rs (constant-product, CPMM-like): documented account ordering, buy/sell discriminators by direction, global_config + protocol-fee accounts, program-id verify, mint vetting, invoke. Register in dispatch. Add to the rounding-mirror fuzz gate.
  - **Done when:** PumpSwap swap CPI works in LiteSVM both directions · predicted==realized fuzz extended to PumpSwap and passes · Program-id verify rejects non-PumpSwap target
- **`onchain-13` Deploy upgradeable + publish verifiable build (Squads authority)** · Fase 2 · 2d · deps: `onchain-11`
  - Deploy the program upgradeable with upgrade authority assigned to the Squads multisig vault; publish the solana-verify reproducible build so on-chain bytecode matches source; write the upgrade runbook. Pin the deployed program id into infra config.
  - **Done when:** solana-verify confirms on-chain hash == source hash · Upgrade authority == Squads multisig (verified on-chain) · Program id pinned in infra config and matches allowlist.rs token/program pins · Runbook documents kill-path: revoke/transfer authority
- **`onchain-14` FORWARD SEAM: PDA-vault / invoke_signed abstraction hook (Fase 3)** · Fase 3 · 1d · deps: `onchain-8`
  - Day-1 design seam ONLY: keep the swap-invoke call behind a thin signer abstraction so a future flash-loan/triangular path that needs a program-owned vault PDA can switch from invoke to invoke_signed without touching adapter ordering logic. No PDA/seeds/rent implemented in Milestone 1 (single-ix atomic inventory needs n…
  - **Done when:** Adapters call swap via an injected signer strategy (default = outer-signer invoke) · README notes PDA vault seeds/rent/re-entrancy are intentionally undefined for Milestone 1 · No CU or behavior regression vs onchain-8 baseline

<details><summary><b>Open questions</b> (6)</summary>

- min_profit semantics: base/priority/tip are SOL-lamport costs not visible as base-asset (WSOL/USDC) balance delta inside the program. Milestone-1 resolution = off-chain sets min_profit to cover all costs+margin in base-asset terms and the assert checks post>=pre+min_profit. Open: if the base asset IS WSOL, should the program additionally read the signer's lamport delta to make cost-coverage on-cha…
- Whether to enforce venue-native min_out (other_amount_threshold / minimum_amount_out) strictly or set it permissive and rely solely on the terminal assert. Current design passes it as belt-and-suspenders; exact value source (off-chain vs derived) is undecided.
- Orca Whirlpool tick-array selection for Fase-1 is client-resolved for a single pair; the convention for >3 tick crossings or multi-pair (Fase 3 CLMM ternary-search sizing) is not yet specified on-chain.
- Exact PumpSwap AMM instruction account layout/discriminators need on-chain verification (less documented than Raydium/Orca) before onchain-12 can be finalized.
- Whether the on-chain Token-2022 vet_mint should be skippable (compute saving) when the off-chain filter is trusted, or always run as defense-in-depth. Current stance: always run (assert is the only safety net), but CU cost vs duplication is an open trade.
- SIMD-0268/0339 activation state on the execution window affects CPI depth/account-info limits; spec says read feature-gate at runtime — the concrete runtime-detection code path and fallback budget are not yet pinned.

</details>

---

### 5.3 `sizing` — Sizing & integer-math engine — CPMM closed-form + bit-exact rounding mirror

**Directory:** `bot/src/sizing/ + bot/src/math/`

**Purpose.** Off-chain integer-math engine that (1) reproduces each Wave-1 venue's on-chain swap arithmetic BIT-EXACT (Raydium CP-Swap and Orca Whirlpool, both swap directions, including fee-only Token-2022 transfer-fee), (2) computes the CPMM round-trip optimal trade size delta* in closed form (general g_a/g_b form + the correct 0.3% reduction) using u128/u256 mul_div with explicit per-venue rounding (Floor output / Ceil required-input), (3) decides whether an opportunity exists via the no-arb predicate, (4) applies the 90-95%-of-optimum sizing policy, and (5) provides Fase-3 seams for CLMM/DLMM ternary/golden-section search and negative-cycle (Bellman-Ford/SPFA) direction discovery. The rounding-mirror…

<details><summary><b>File map</b> (21 files)</summary>

- `bot/src/math/mod.rs` — Public surface of the integer-math primitives crate-module.
- `bot/src/math/u256.rs` — 256-bit unsigned integer used as the wide intermediate for reserve_out * amount_in_after_fee and for the sqrt(Ra_in·Ra_out·Rb_in·Rb_out) product (four u64 multiplied = up…
- `bot/src/math/mul_div.rs` — mul_div_floor / mul_div_ceil over u128 with u256 intermediate.
- `bot/src/math/rounding.rs` — RoundDirection enum + helpers encoding the §7 rule: Floor for output, Ceil for required-input, always favor the pool.
- `bot/src/math/fees.rs` — Token-2022 TransferFee forward and inverse math (non-symmetric, floor division, ≤1 unit apart) plus CPMM swap-fee bps helpers.
- `bot/src/sizing/mod.rs` — Public surface of the sizing engine.
- `bot/src/sizing/quoter.rs` — Core trait + data types every venue implements.
- `bot/src/sizing/venues/mod.rs` — Venue registry + the allowlisted program ids (mirror of on-chain allowlist) so off-chain sizing and on-chain assert agree on which venue math applies.
- `bot/src/sizing/venues/raydium_cpmm.rs` — Raydium CP-Swap (x*y=k) Quoter impl, bit-exact to raydium-cp-swap curve.
- `bot/src/sizing/venues/orca_whirlpool.rs` — Orca Whirlpool CPMM-leg Quoter impl for Milestone-1 (single price-range / no tick crossing path) bit-exact to whirlpool's get_amount_delta / fee math.
- `bot/src/sizing/venues/pumpswap.rs` — PumpSwap AMM (constant-product, pump.fun graduated pool) Quoter impl.
- `bot/src/sizing/roundtrip.rs` — Composite-CPMM round-trip model: input base X -> pool A -> token Y -> pool B -> back to X.
- `bot/src/sizing/cpmm_optimal.rs` — The closed-form delta* — both the general g_a/g_b form (§7 line 322) and the spec-endorsed 0.3% reduction (§7 line 329), in u128/u256 with isqrt.
- `bot/src/sizing/opportunity.rs` — Opportunity-exists predicate (§7 line 334): g_a·g_b·Ra_out·Rb_out > Ra_in·Rb_in, evaluated in integers with no overflow.
- `bot/src/sizing/policy.rs` — 90-95%-of-optimum sizing policy (§7 line 337, §11 line 514).
- `bot/src/sizing/search.rs` — FASE-3 SEAM (compile-present, gated).
- `bot/src/sizing/cycle.rs` — FASE-3 SEAM (compile-present, gated).
- `bot/src/sizing/error.rs` — Error enum shared by quoters/optimizer.
- `bot/src/sizing/fixtures/mod.rs` — Numeric worked-example fixtures (§7 mandate: tambahkan contoh numerik reserves -> delta* sebagai unit test).
- `bot/src/sizing/tests/differential.rs` — The Milestone-1 GATE: per-venue, both-direction property/fuzz test asserting predicted_out == on-chain realized_out across a wide (reserves, fee, amount_in) range, includ…
- `bot/src/sizing/Cargo-fragment.toml` — Dependency fragment to merge into bot/Cargo.toml documenting pinned crates this module needs.

</details>

**Public interfaces (14):**

```rust
// RoundDirection — Encodes §7 rule: Floor for output amounts, Ceil for required-input.
pub enum RoundDirection { Floor, Ceil } impl RoundDirection { pub fn apply(self, a: u128, b: u128, denom: u128) -> Option<u128> }
// mul_div_floor — (a*b)/denom via U256 intermediate, floored.
pub fn mul_div_floor(a: u128, b: u128, denom: u128) -> Option<u128>
// mul_div_ceil — Ceiling variant for required-input sizing.
pub fn mul_div_ceil(a: u128, b: u128, denom: u128) -> Option<u128>
// isqrt — Deterministic integer floor square root (Newton).
pub fn isqrt(x: U256) -> U256
// transfer_fee_forward — Token-2022 forward fee = min(floor(amount*bps/10000), maximum_fee).
pub fn transfer_fee_forward(cfg: &TransferFeeConfig, pre_fee_amount: u64) -> u64
// transfer_fee_inverse — Inverse (gross-up) fee, mirrors calculate_inverse_fee.
pub fn transfer_fee_inverse(cfg: &TransferFeeConfig, post_fee_amount: u64) -> u64
// Quoter — The day-1 venue abstraction.
pub trait Quoter { fn quote_exact_in(&self, q: &QuoteIn) -> Result<QuoteOut, QuoteError>; fn quote_required_in(&self, desired_out: u64, dir: SwapDir) -> Result<u64, QuoteError>; fn marginal_price_x64(&self, dir: SwapDir) -> u128; }
// optimal_delta_general — §7 general closed form delta* = (isqrt(g_a·g_b·Ra_in·Ra_out·Rb_in·Rb_out) − Ra_in·Rb_in)/(g_a·Rb_in + g_a·g_b·Ra_out) in scaled integers.
pub fn optimal_delta_general(r: &CpmmReserves) -> Option<u128>
// optimal_delta_fee30bps — §7 CORRECT 0.3% reduction: (997·isqrt(Ra_in·Ra_out·Rb_in·Rb_out) − 1000·Ra_in·Rb_in)/((994009·Ra_out + 997000·Rb_in)/1000).
pub fn optimal_delta_fee30bps(ra_in: u128, ra_out: u128, rb_in: u128, rb_out: u128) -> Option<u128>
// opportunity_exists — §7 predicate g_a·g_b·Ra_out·Rb_out > Ra_in·Rb_in via cross-multiplied U256 (no division).
pub fn opportunity_exists(r: &CpmmReserves) -> bool
// size_trade — End-to-end sizing: opportunity gate -> delta* -> clamp to 90-95% & max inventory -> re-quote round-trip via real quoters -> predicted profit.
pub fn size_trade(rt: &RoundTrip, policy: &SizingPolicy) -> Option<SizedTrade>
// RoundTrip::realized_out — Runs both legs through the real bit-exact Quoters — the ground-truth round-trip output used for prediction and for differential-testing the optimizer.
pub fn realized_out(&self, delta_in: u64) -> Result<u64, QuoteError>
// golden_section_max — Fase-3 seam: unimodal integer golden-section search over input size for CLMM/DLMM (cap <=30 iters).
#[cfg(feature="fase3")] pub fn golden_section_max<F: Fn(u64) -> Option<i128>>(profit: F, cfg: &SearchCfg) -> Option<(u64, i128)>
// find_negative_cycle — Fase-3 seam: Bellman-Ford/SPFA over -ln(rate) weights returning arbitrage DIRECTION ONLY.
#[cfg(feature="fase3")] pub fn find_negative_cycle(n_nodes: usize, edges: &[Edge]) -> Option<Vec<Edge>>
```

**Key data structures:**

- **`CpmmReserves`** — `struct { ra_in:u128, ra_out:u128, rb_in:u128, rb_out:u128, g_a_num:u128, g_a_den:u128, g_b_num:u128, g_b_den:u128 }` · The round-trip composite inputs for the closed form.
- **`QuoteIn / QuoteOut`** — `QuoteIn { amount_in:u64, dir:SwapDir, transfer_fee_in:Option<TransferFeeConfig>, transfer_fee_out:Option<TransferFeeConfig> }  QuoteOut { amount_out:u64, fee_pa…` · net_* fields carry the ACTUAL balance-delta amounts (post Token-2022 transfer fee), which is what on-chain profit-check reads pre/post (§9).
- **`SwapDir`** — `enum { AtoB, BtoA }` · Both directions are first-class because Raydium and Orca arithmetic and rounding differ per direction;
- **`TransferFeeConfig`** — `struct { transfer_fee_basis_points:u16, maximum_fee:u64, epoch:u64 }` · epoch field forces a live per-epoch fee read (§9: getEpochFee, never cache across epoch).
- **`RaydiumCpmm`** — `struct { reserve_in:u64, reserve_out:u64, trade_fee_rate:u64, fee_denom:u64 }` · trade_fee_rate over fee_denom (1_000_000).
- **`OrcaWhirlpool`** — `struct { sqrt_price_x64:u128, liquidity:u128, fee_rate:u16 }` · Q64.64 sqrtPrice, fee_rate denom 1_000_000.
- **`PumpSwap`** — `struct { reserve_in:u64, reserve_out:u64, lp_fee_bps:u64, protocol_fee_bps:u64 }` · Wave-1 constant-product venue.
- **`SizingPolicy / SizedTrade`** — `SizingPolicy { fraction_num:u32, fraction_den:u32, max_inventory:u64, min_profit:u64 }  SizedTrade { size_in:u64, predicted_gross_out:u64, predicted_profit:u64,…` · fraction default 92/100 (within 90-95).
- **`Edge (cycle)`** — `#[cfg(feature="fase3")] struct { from:u32, to:u32, venue_idx:usize, dir:SwapDir, neg_log_rate_q32:i64 }` · Fase-3.
- **`QuoteError`** — `enum { Overflow, ZeroReserve, CrossesTick, NoOpportunity, InventoryExceeded, EpochFeeStale }` · Overflow surfaces u256-narrowing failures;

**External crates:** `ruint = "1" (U256 wide intermediate + isqrt)`, `thiserror = "1" (QuoteError)`, `solana-program (Pubkey for the venue allowlist constants; matches on-chain pin)`, `proptest = "1" (dev-only, the fuzz/property gate)`, `spl-token-2022 (dev-only, cross-check transfer-fee math against the canonical calculate_fee/calculate_inverse_fee)`, `litesvm (dev-only, via the on-chain module's exported CPI harness for the differential gate)`

**Grounded in `plan.md`:** §7 (lines 313-364): closed-form delta* general + 0.3% reduction, Floor/Ceil rounding, opportunity predicate, strictly-concave/flat-near-optimum policy, CLMM golden-section seam, negative-cycle direction-only seam; §11 Fase 1 (lines 505-533): sizing engine deliverable, 90-95% policy, the WAJIB per-venue both-direction fuzz/property rounding-mirror test that GATES Milestone 1; §4 (lines 158-176): venue math characteristics (Raydium CPMM x*y=k vs Orca Whirlpool sqrtPriceX64 — implement per-venue); §9 (referenced lines 284,437-440): Token-2022 forward vs inverse fee non-symmetry, floor division, live per-epoch fee, profit from actual balance delta

**Tasks (10):**

- **`sizing-1` Wide integer-math primitives: U256, mul_div, rounding** · Fase 0 · 1.5d · deps: —
  - Stand up bot/src/math: U256 alias over ruint with checked_mul/isqrt/narrowing, mul_div_floor/mul_div_ceil routing through U256, and the RoundDirection enum. No f64 anywhere. This is the chokepoint every venue and the optimizer route through.
  - **Done when:** mul_div_ceil(a,b,d) == mul_div_floor(a,b,d) + ((a*b)%d != 0) holds over proptest range · isqrt(x)^2 <= x < (isqrt(x)+1)^2 for x up to U256::MAX over proptest · mul_div returns None (not panic) on denom==0 and on u128-narrowing overflow · grep confirms zero f64/f32 usage in bot/src/math
- **`sizing-2` Token-2022 transfer-fee forward/inverse math** · Fase 1 · 1d · deps: `sizing-1`
  - Implement transfer_fee_forward and transfer_fee_inverse mirroring spl-token-2022 calculate_fee/calculate_inverse_fee exactly, plus net_after_transfer_fee. Carry epoch in TransferFeeConfig and reject stale-epoch use. Forward and inverse are non-symmetric (≤1 unit, floor division) and must never be swapped.
  - **Done when:** transfer_fee_forward == spl_token_2022::extension::transfer_fee::calculate_fee across proptest (bps 0..=10000, amounts wide, maximum_fee cap exercised) · transfer_fee_inverse == calculate_inverse_fee across proptest · Documented asymmetry: forward(inverse(x)) and inverse(forward(x)) differ by <=1 and test pins it · Using a TransferFeeConfig whose epoch != current yields QuoteError::EpochFeeStale
- **`sizing-3` Quoter trait + QuoteIn/QuoteOut/SwapDir/QuoteError + venue registry** · Fase 1 · 1d · deps: `sizing-1`, `sizing-2`
  - Define the day-1 venue abstraction (Quoter trait with quote_exact_in / quote_required_in / marginal_price_x64), the I/O structs carrying both gross and net (post-transfer-fee balance-delta) amounts, the error enum, and the allowlisted program-id constants mirroring the on-chain allowlist.
  - **Done when:** Trait compiles and is object-safe (used as &dyn Quoter in RoundTrip) · QuoteOut exposes net_in/net_out distinct from gross so callers profit-check on actual balance delta · Program-id constants byte-equal the on-chain allowlist (shared test asserts equality)
- **`sizing-4` Raydium CP-Swap Quoter (bit-exact)** · Fase 1 · 1.5d · deps: `sizing-3`
  - Implement RaydiumCpmm Quoter reproducing raydium-cp-swap curve math: input-fee then x*y=k, Floor output, Ceil required-input, get_transfer_fee on input / get_transfer_inverse_fee on output sizing per §9. Both directions.
  - **Done when:** quote_exact_in matches hand-computed §7 Raydium fixtures for AtoB and BtoA · quote_required_in(quote_exact_in(x).amount_out) >= x (Ceil never under-asks) · Token-2022 fee path: net_out == gross_out - transfer_fee_forward(out)
- **`sizing-5` Orca Whirlpool Quoter (bit-exact, in-range)** · Fase 1 · 2.5d · deps: `sizing-3`
  - Implement OrcaWhirlpool Quoter for the Milestone-1 in-current-tick-array path: Q64.64 sqrtPrice get_next_sqrt_price + get_amount_delta_a/b with per-direction RoundDirection, fee applied to input in Whirlpool order, Floor output / Ceil input. Return QuoteError::CrossesTick when the swap would cross an initialized tick (…
  - **Done when:** quote_exact_in matches whirlpool reference math for AtoB and BtoA on in-range fixtures · Rounding direction differs correctly between a->b and b->a (pinned by fixtures) · Swap exceeding current liquidity range returns CrossesTick rather than a wrong number · Implemented in its own module, no code shared with raydium_cpmm (per §11)
- **`sizing-6` PumpSwap AMM Quoter (bit-exact)** · Fase 1 · 1d · deps: `sizing-3`
  - Implement PumpSwap constant-product Quoter (lp+protocol fee pre-swap, x*y=k Floor output) for the Wave-1 PumpSwap venue.
  - **Done when:** quote_exact_in matches PumpSwap on-chain math both directions on fixtures · Combined lp+protocol fee applied exactly once, pre-swap
- **`sizing-7` RoundTrip composite + CpmmReserves extraction** · Fase 1 · 1d · deps: `sizing-4`, `sizing-5`, `sizing-6`
  - Implement RoundTrip (poolA,dirA,poolB,dirB), extract_cpmm_reserves to build the CpmmReserves tuple from concrete Quoters, and realized_out that runs both legs through the real bit-exact quoters (the ground truth for prediction and optimizer testing).
  - **Done when:** realized_out(size) equals manual two-leg composition on fixtures · extract_cpmm_reserves yields g as num/den matching each venue's fee · Round-trips two CPMM venues; for Whirlpool uses its effective in-range reserves
- **`sizing-8` Closed-form delta* (general + 0.3% reduction) + opportunity predicate + policy** · Fase 1 · 2d · deps: `sizing-7`
  - Implement optimal_delta_general and optimal_delta_fee30bps in u128/u256 with isqrt (spec-endorsed reduction, NOT the donggeunyu form; 994.009 expressed as 994009/1000), opportunity_exists, and size_trade applying the 90-95% clamp + max-inventory + min-profit, re-quoting via real quoters so predicted profit == on-chain…
  - **Done when:** optimal_delta_general(g=0.997) == optimal_delta_fee30bps within documented ±1 on all fixtures · Numeric reserves->delta* fixtures (§7 mandate) present incl. a no-arb case returning None · opportunity_exists true iff delta* numerator>0; agrees with g_a·g_b·Ra_out·Rb_out > Ra_in·Rb_in · size_trade clamps to fraction in [90,95]% and to max_inventory; constructor rejects out-of-band fractions · predicted_profit from size_trade equals RoundTrip::realized_out(size)-size exactly · brute-force max over a sampled size grid confirms 90-95%-clamped size captures profit within the concave plateau
- **`sizing-9` GATE: per-venue both-direction differential/property test (Milestone-1 gate)** · Fase 1 · 2.5d · deps: `sizing-8`, `onchain-<TryArbitrage LiteSVM CPI harness exporting realized_out for (pool,amount_in,dir)>`
  - Build the proptest harness asserting predicted_out == on-chain realized_out across a wide (reserves, fee, amount_in) range for Raydium, Orca, and PumpSwap, both SwapDir, including Token-2022 fee path. Consumes the on-chain TryArbitrage LiteSVM CPI harness to capture realized_out. This test GATES Milestone 1.
  - **Done when:** For each venue and both directions, quote_exact_in().net_out == on-chain realized balance delta over the full fuzz range (zero counterexamples) · Token-2022 fee-only path included and passes · quote_required_in round-trips with Ceil (never under-asks) over the range · Test wired into the CI gate job that blocks Milestone-1 sign-off
- **`sizing-10` FASE-3 SEAM: golden-section search + Bellman-Ford cycle (compile-present, gated)** · Fase 3 · 2d · deps: `sizing-8`
  - Implement golden_section_max (unimodal integer search, <=30 iters) and find_negative_cycle (Bellman-Ford/SPFA over -ln-rate weights, direction-only) behind feature="fase3", against the existing Quoter trait, with explicit re-size enforcement. ONLY the day-1 trait seam (marginal_price_x64, required_in) is built in Miles…
  - **Done when:** golden_section_max finds the optimum of a synthetic unimodal profit within tolerance in <=30 evals · find_negative_cycle recovers a planted negative cycle and returns ordered Edges (direction only) · Caller path asserts every cycle leg is re-sized via size_trade before any tx is built (no trade-on-cycle-without-resize) · Default (non-fase3) build excludes both modules and Milestone-1 code never references them

<details><summary><b>Open questions</b> (6)</summary>

- Whirlpool effective-reserve linearization for the CPMM closed form: Milestone-1 assumes the optimal trade stays within the current tick range so Ra/Rb can be derived from (liquidity, sqrtPrice). The spec leaves undefined what to do when delta* would cross a tick — current design returns CrossesTick and falls back (Fase-3) to golden-section. Confirm this fallback is acceptable for Milestone-1 thin…
- Exact Raydium CP-Swap and PumpSwap fee numerator/denominator conventions (trade_fee_rate scale, whether protocol fee is separate) must be pinned from the live programs before sizing-4/sizing-6 are declared bit-exact; spec gives x*y=k but not the fee field layout.
- Default policy fraction within 90-95%: design defaults to 92/100. Spec says 90-95 without a single value; confirm whether thin-pool niche wants the high end (95) to grab more of a small profit.
- min_profit unit and source: should it be a fixed lamport floor, or derived from current priority-fee+tip estimate per opportunity? The on-chain assert uses min_profit+costs; sizing needs the same number to avoid predicting a trade the chain reverts.
- Whether marginal_price_x64 for cycle weights (Fase-3) should be size-0 spot or a small-epsilon marginal — spec says size-0 marginal but that ignores fee at the margin; decide the fee treatment when Fase-3 starts.
- Token-2022 transfer-fee on intermediate token Y in a round-trip: confirm both legs read live epoch fee for Y's mint and that inverse-vs-forward selection is correct per leg (input leg forward, required-input-for-target inverse).

</details>

---

### 5.4 `detection` — Detection — Yellowstone gRPC ingest, idempotent pool-state cache, token-pair graph

**Directory:** `bot/src/detection/`

**Purpose.** Detection layer for the atomic-arbitrage bot: ingest pool-state writes from Yellowstone gRPC (Geyser) at `processed` commitment, decode Wave-1 venue account layouts (Raydium CPMM vaults+PoolState, Orca Whirlpool sqrtPriceX64, PumpSwap AMM), maintain an idempotent in-memory pool-state cache (keyed by pool pubkey, deduped by slot + write_version intra-session), incrementally recompute a token-pair graph of candidate edges, and emit dislocation candidates to the downstream sizing/profit-gate layer. Survives reconnect/replay (resubscribe from last processed slot; on the first post-reconnect update prefer higher slot unconditionally because write_version is incomparable across sessions). Designed…

<details><summary><b>File map</b> (12 files)</summary>

- `bot/src/detection/mod.rs` — Crate-internal module root for detection.
- `bot/src/detection/config.rs` — Strongly-typed detection config + loader.
- `bot/src/detection/model.rs` — Core shared data types: Venue enum, PoolState enum (RaydiumCpmm/OrcaWhirlpool/PumpSwap variants), PoolKey, normalized Reserves/SqrtPrice price view, AccountUpdate (the de…
- `bot/src/detection/grpc.rs` — Yellowstone gRPC client wrapper.
- `bot/src/detection/decode.rs` — Per-venue zero-copy decoders.
- `bot/src/detection/cache.rs` — Idempotent in-memory pool-state cache keyed by pool pubkey.
- `bot/src/detection/graph.rs` — Token-pair graph with incremental edge recompute.
- `bot/src/detection/reconnect.rs` — Reconnect/replay supervisor.
- `bot/src/detection/metrics.rs` — Detection-layer instrumentation: account-update throughput, decode-error rate, cache accept/reject (dedupe) counters, per-pool freshness/staleness, reconnect count, gap-r…
- `bot/src/detection/discovery.rs` — FASE 3 FORWARD HOOK (day-1 seam only): owner-firehose pool auto-discovery + graduation detection via tx-subscribe to migration authority.
- `bot/src/detection/tests/mod.rs` — Unit + property tests for the detection module: cache idempotency (dedupe slot+write_version), reconnect higher-slot-unconditional rule, multi-component CPMM assembly, de…
- `bot/src/detection/fixtures/README.md` — Documents how to capture golden account-byte fixtures from mainnet (solana account --output json-compact / Surfpool clone) for the 3 Wave-1 venues, used by decoder tests…

</details>

**Public interfaces (10):**

```rust
// spawn_detection — Top-level entrypoint other modules call.
pub fn spawn_detection(cfg: DetectionConfig, rpc: Arc<dyn RpcReconciler>) -> anyhow::Result<DetectionHandle>
// DetectionHandle — Handle the sizing/profit-gate layer holds.
pub struct DetectionHandle { pub signals: tokio::sync::broadcast::Receiver<DetectionSignal>, pub cache: Arc<PoolStateCache>, pub graph: Arc<RwLock<PairGraph>> } impl DetectionHandle { pub fn subscribe(&self) -> broadcast::Receiver<DetectionSignal>; pub fn snapshot_pool(&self, pool: &Pubkey) -> Option<PriceView>; pub fn last_processed_slot(&self) -> u64 }
// DetectionSignal — The downstream-facing event.
pub enum DetectionSignal { EdgeUpdated(EdgeUpdate), PoolStale(Pubkey) }
// EdgeUpdate — Emitted whenever an accepted state change moves an edge.
pub struct EdgeUpdate { pub pair: (Pubkey, Pubkey), pub pools: Vec<PoolQuote>, pub best_spread_bps: i64, pub max_slot: u64 } pub struct PoolQuote { pub pool: Pubkey, pub venue: Venue, pub price: PriceView, pub stamp: SessionStamp }
// PoolStateCache::apply — Idempotent merge.
pub fn apply(&self, upd: DecodedComponent, session_epoch: u64) -> Option<DetectionEvent>
// PoolStateCache::accept_predicate — The canonical dedupe/reconnect rule resolved from §5: within a session compare slot then write_version;
fn accept(new: &SessionStamp, cached: &SessionStamp) -> bool { if new.session_epoch != cached.session_epoch { new.slot >= cached.slot } else { new.slot > cached.slot || (new.slot == cached.slot && new.write_version > cached.write_version) } }
// PoolDecoder — Per-venue decoder.
pub trait PoolDecoder { fn venue(&self) -> Venue; fn decode(&self, owner: &Pubkey, pubkey: &Pubkey, data: &[u8], stamp: SessionStamp) -> Result<DecodedComponent, DecodeError>; }
// PairGraph::on_event — Incremental recompute: updates only the single edge bucket keyed by the event pool's mint pair and returns an EdgeUpdate iff that bucket's best spread…
pub fn on_event(&mut self, ev: &DetectionEvent) -> Option<EdgeUpdate>
// RpcReconciler — Implemented by the shared RPC layer (external module).
pub trait RpcReconciler: Send + Sync { async fn refetch(&self, pools: &[Pubkey]) -> anyhow::Result<Vec<DecodedComponent>>; }
// PoolDiscovery — FASE 3 forward-hook trait.
pub trait PoolDiscovery: Send + Sync { fn discovered(&self) -> tokio::sync::mpsc::Receiver<NewPool>; } pub struct NewPool { pub pool: Pubkey, pub venue: Venue, pub mints: (Pubkey, Pubkey), pub vaults: Vec<Pubkey> }
```

**Key data structures:**

- **`SessionStamp`** — `struct SessionStamp { session_epoch: u64, slot: u64, write_version: u64 }` · INVARIANT: write_version is only meaningful/comparable when session_epoch matches.
- **`Venue`** — `enum Venue { RaydiumCpmm, OrcaWhirlpool, PumpSwap }` · Wave-1 only (§4).
- **`DecodedComponent`** — `enum DecodedComponent { CpmmPoolState { pool: Pubkey, base_mint: Pubkey, quote_mint: Pubkey, base_vault: Pubkey, quote_vault: Pubkey, fee_bps: u32, stamp: Sessi…` · Output of decode.rs, input to cache.apply.
- **`PoolEntry`** — `struct PoolEntry { venue: Venue, base_mint: Pubkey, quote_mint: Pubkey, last_stamp: SessionStamp, freshness: Freshness, // CPMM assembly state: cpmm: Option<Cpm…` · INVARIANT: a CPMM pool only becomes Fresh once PoolState + both vault amounts are present AND their slots are within a bounded skew (config max_component_slot_skew).
- **`PriceView`** — `struct PriceView { base_mint: Pubkey, quote_mint: Pubkey, reserve_base: u128, reserve_quote: u128, sqrt_price_x64: Option<u128>, fee_bps: u32 }` · Normalized advisory price used by graph spread-detection and handed to the sizer as a starting point.
- **`EdgeBucket`** — `struct EdgeBucket { pools: SmallVec<[PoolQuote; 4]>, best_spread_bps: i64, last_emit_spread_bps: i64 }` · One per unordered mint pair.
- **`DetectionConfig`** — `struct DetectionConfig { endpoint: String, x_token: Option<String>, commitment: CommitmentLevel /*=Processed*/, venues: Vec<VenueSubscription>, reconnect: Recon…` · INVARIANT (Chainstack gotcha §5): SubMode::OwnerFirehose MUST NOT be used for any subscription whose target accounts are SPL Token-program-owned vaults (Chainstack blocks owner-subscribe to TokenkegQ.…
- **`ReconnectConfig`** — `struct ReconnectConfig { base_backoff_ms: u64, max_backoff_ms: u64, jitter_frac: f64, replay_buffer_slots: u64 /*~100 Chainstack*/, reconcile_on_gap: bool }` · On reconnect: from_slot=last_processed_slot;

**External crates:** `yellowstone-grpc-client (rpcpool/yellowstone-grpc, Dragon's Mouth; AGPL-3.0 — note license obligation in plan §5)`, `yellowstone-grpc-proto`, `solana-sdk / solana-program (Agave; Pubkey, CommitmentLevel)`, `bytemuck (zero-copy #[repr(C)] casts for vault amount offset-64 and Anchor offset-8 structs, §5)`, `tokio (async runtime, broadcast/mpsc channels)`, `dashmap (concurrent pool cache)`, `smallvec (EdgeBucket pools)`, `tonic + prost (gRPC transport under yellowstone client)`, `anyhow / thiserror (errors)`, `tracing (instrumentation)`, `prometheus or metrics (counters/histograms — match bot-wide choice)`, `proptest (cache ordering + dedupe property tests)`

**Grounded in `plan.md`:** §5 (180-229) detection layer; §4 (120-177) venues/program IDs/per-pool subscribed accounts; §8 (384-398) data-source ladder & reconnect resilience; §1-3 (7-117) system context; §11 (480-609) phase roadmap for task phasing

**Tasks (10):**

- **`detection-1` Detection config + venue program-id verification** · Fase 0 · 1.5d · deps: —
  - Define DetectionConfig / VenueSubscription / SubMode / ReconnectConfig and a TOML loader under infra/. Pin the 3 Wave-1 program IDs and verify each on-chain (Solscan + getAccountInfo) per §4. Encode the Chainstack guardrail: reject any config where OwnerFirehose targets SPL-vault accounts.
  - **Done when:** All 3 Wave-1 program IDs verified on-chain and pinned in config · commitment defaults to Processed and cannot be set to a weaker latency mode without explicit override · Config rejects OwnerFirehose on any SPL-Token-program-owned vault subscription (Chainstack gotcha) · Loader round-trips a Fase-1 single-pair config
- **`detection-2` Core model + SessionStamp dedupe types** · Fase 1 · 1d · deps: `detection-1`
  - Implement model.rs: Venue, SessionStamp (with session_epoch), DecodedComponent, PriceView, DetectionEvent, DetectionSignal, EdgeUpdate, PoolQuote. Establish the cross-session write_version-incomparability invariant in types/docs.
  - **Done when:** SessionStamp carries session_epoch+slot+write_version · Types compile and are re-exported via mod.rs · Doc-comment on SessionStamp states write_version is intra-session-only
- **`detection-3` Per-venue decoders (CPMM vaults+PoolState, Whirlpool, PumpSwap)** · Fase 1 · 4d · deps: `detection-2`
  - Implement decode.rs PoolDecoder impls. Raydium CPMM: verify Anchor 8-byte discriminator then read PoolState (mints, vault pubkeys, fee) from offset 8; read SPL vault amount at offset 64 (NOT Anchor). Orca Whirlpool: verify discriminator, read sqrtPriceX64 (Q64.64), tickSpacing, feeRate from offset 8. PumpSwap: constant…
  - **Done when:** Whirlpool decode reads sqrtPriceX64 from offset 8 only after discriminator match; wrong discriminator returns BadDiscriminator · SPL vault amount read from offset 64 matches a known on-chain vault · CPMM PoolState yields both vault pubkeys + both mints + fee · PumpSwap reserves decode validated against a cloned mainnet PumpSwap pool · All decoders tested against real cloned account bytes, not synthetic
- **`detection-4` Idempotent pool-state cache + CPMM multi-component assembly** · Fase 1 · 3d · deps: `detection-3`
  - Implement cache.rs: PoolStateCache with accept_predicate (slot+write_version intra-session; higher-or-equal slot unconditionally across epochs), vault->pool reverse index, CPMM assembly (PoolState + 2 vaults within bounded slot skew before Fresh), bump_epoch, and DetectionEvent emission only on accepted+complete+materi…
  - **Done when:** Lower write_version at same slot+epoch is dropped · After epoch bump, a higher slot is accepted even if its write_version is numerically lower (cross-session rule) · CPMM pool not marked Fresh until PoolState + both vault amounts present within max_component_slot_skew · apply() returns None on idempotent re-delivery of an already-applied stamp · Cache holds no disk persistence (intra-session only)
- **`detection-5` Yellowstone gRPC ingest client** · Fase 1 · 2.5d · deps: `detection-2`
  - Implement grpc.rs: build SubscribeRequest (accounts filter by owner OR explicit list + memcmp/datasize + accounts_data_slice), commitment=processed, from_slot threading, bidi stream demux into RawAccountUpdate (slot, write_version, pubkey, owner, data). Pure transport. Validate against Chainstack Yellowstone gRPC endpo…
  - **Done when:** SubscribeRequest sets commitment=processed and applies accounts_data_slice to trim payload · Targeted mode subscribes explicit pubkeys; OwnerFirehose mode subscribes by program owner · Stream surfaces slot+write_version+owner+data per update · from_slot is honored on (re)subscribe · Smoke test receives live account updates for the configured pair
- **`detection-6` Token-pair graph + incremental edge recompute** · Fase 1 · 2d · deps: `detection-4`
  - Implement graph.rs: adjacency keyed by unordered mint pair, pool_to_pair index, EdgeBucket with best_spread_bps + debounce. on_event recomputes ONLY the affected edge and emits EdgeUpdate when spread crosses emit_threshold_bps with min_delta_bps movement (no full re-scan, §5).
  - **Done when:** A single DetectionEvent mutates exactly one EdgeBucket · best_spread_bps correctly reflects max cross-pool divergence for the Fase-1 CPMM+Whirlpool pair · EdgeUpdate emitted only above threshold and after min_delta movement (debounced) · No full-graph re-scan occurs on update (asserted via counter)
- **`detection-7` Reconnect/replay supervisor + run-loop wiring** · Fase 1 · 3d · deps: `detection-4`, `detection-5`, `detection-6`
  - Implement reconnect.rs + the grpc->decode->cache->graph run-loop. Track last_processed_slot; on disconnect bump session_epoch, resubscribe with from_slot=last_processed_slot (same filter+commitment), apply jittered capped backoff. On gap > replay_buffer_slots, call RpcReconciler.refetch to backfill (Chainstack ~100-slo…
  - **Done when:** On reconnect, session_epoch is bumped before resubscribe and from_slot=last_processed_slot · First post-reconnect update for each pool applies the higher-slot-unconditional rule (no spurious drop) · Gap exceeding replay_buffer_slots triggers RpcReconciler backfill · Backoff is exponential, jittered, and capped · DetectionHandle delivers DetectionSignal to a subscriber and snapshot_pool returns current PriceView
- **`detection-8` Detection metrics + latency instrumentation** · Fase 2 · 1.5d · deps: `detection-7`
  - Implement metrics.rs: updates_total, decode_errors_total{venue}, cache_rejected_total{reason}, reconnects_total, gap_reconciles_total, hot/stale pool gauges, and ingest_to_edge latency histogram (P50/P95). Register into the bot-wide registry. Detection latency is a first-class health metric (§5, success criteria §1).
  - **Done when:** ingest_to_edge latency histogram exposes P50/P95 · Dedupe rejects and reconnects are counted · Per-venue decode-error counter increments on bad discriminator · Metrics register without duplicate-registration panics into the shared registry
- **`detection-9` Fase-2 targeted subscription sizing (20-50 pairs) + PumpSwap integration** · Fase 2 · 2.5d · deps: `detection-3`, `detection-7`
  - Scale config + ingest to ~20-50 targeted pairs (~80-200 accounts) within Chainstack 50-accounts/stream caps (multi-stream split), and wire PumpSwap AMM as a live venue (§4 KOREKSI: graduations now go to PumpSwap, not Raydium). Keep CPMM/PumpSwap vaults Targeted (Chainstack SPL-owner block). Validate on Chainstack Yello…
  - **Done when:** 20-50 targeted pairs subscribe across streams without exceeding Chainstack 50-account/stream cap · PumpSwap pools decode + emit EdgeUpdate alongside Raydium CPMM and Orca Whirlpool · No subscription owner-firehoses an SPL-owned vault · End-to-end run on Chainstack gRPC sustains updates for the full pair set
- **`detection-10` FASE 3 forward-hook: owner-firehose discovery seam** · Fase 3 · 1d · deps: `detection-4`
  - DAY-1 SEAM ONLY (implemented minimally, marked forward-hook): define PoolDiscovery trait + NewPool + cache.register_pool so dynamically-discovered pools can enter cache/graph without redesign. Ship StaticDiscovery for Fase 1/2; leave FirehoseDiscovery (owner-firehose + migration-authority tx-sub for graduation detectio…
  - **Done when:** PoolDiscovery trait exists and StaticDiscovery feeds the configured Fase-2 set through it · cache.register_pool admits a NewPool (with vaults) without restructuring existing entries · FirehoseDiscovery is clearly marked as a Fase-3 placeholder, not on the Milestone-1 path

<details><summary><b>Open questions</b> (7)</summary>

- Whirlpool-vs-CPMM price normalization unit: detection emits best_spread_bps, but the exact normalized price basis (and whether spread should be computed off raw sqrt-price or a derived spot price including fee) is shared with the sizing module — needs a contract agreement so detection's advisory spread matches the sizer's closed-form view.
- PumpSwap PoolState exact field offsets and fee model (constant-product fee bps location) are under-documented in the spec; must be confirmed against cloned mainnet bytes before detection-3 trusts them.
- max_component_slot_skew for CPMM assembly: how stale can a vault be relative to PoolState before the pool is treated as Pending rather than Fresh? Needs an empirically-tuned default (proposed: a few slots).
- emit_threshold_bps / min_delta_bps defaults: where to set the dislocation gate so detection shortlists usefully without flooding the sizer — depends on the niche pair liquidity profile (§1 thin/fresh pools).
- RpcReconciler ownership: is gap-backfill getMultipleAccounts owned by a shared rpc module or implemented inside detection? Plan implies a shared rpc layer; confirm the module boundary and that it returns DecodedComponent-compatible data.
- Whether to subscribe Whirlpool tick-array PDAs at all for Wave-1: spec says fetch on-demand (PDA, +0 subscriptions), but if the sizer needs tick liquidity for accurate sizing, an on-demand RPC fetch path may need to live adjacent to detection.
- Broadcast channel back-pressure policy: if the sizer lags, do we drop oldest EdgeUpdates (lossy, latency-first) or block? Latency-first argues lossy, but needs an explicit decision.

</details>

---

### 5.5 `txbuilder` — Tx builder & ALT — v0 assembly, ALT lifecycle, compute budget, WSOL, Token-2022 filter

**Directory:** `bot/src/txbuilder/ + infra/alt/`

**Purpose.** Assemble the single all-or-nothing v0 VersionedTransaction for atomic arb [ComputeBudget(limit+price), optional WSOL wrap/sync, swap1, swap2, profit_assert, jito_tip], own the Address-Lookup-Table lifecycle (create/extend ~30 keys/tx, 1-slot warm-up, static long-lived table + optional per-route, janitor close after ~512 slots), enforce all hard limits (<=128 account locks, <=1232 bytes, <=1.4M CU, <=256 loaded accounts, signers not in ALT), compute SetComputeUnitLimit from measured CU + ~10% / SetComputeUnitPrice, run the WSOL createIdempotent->transfer->SyncNative->CloseAccount dance, HARD-REJECT Token-2022 mints carrying dangerous extensions, and run preflight simulateTransaction (replaceR…

<details><summary><b>File map</b> (21 files)</summary>

- `bot/src/txbuilder/mod.rs` — Module root.
- `bot/src/txbuilder/builder.rs` — Core assembler: takes an ArbTxPlan (sizing output + resolved account metas + ALT handles) and produces a v0 VersionedMessage with the fixed instruction order, enforcing a…
- `bot/src/txbuilder/layout.rs` — Declares the canonical instruction-slot enum and the fixed ordering so other modules and tests agree on positions;
- `bot/src/txbuilder/limits.rs` — Single source of truth for hard limits and the validate() gate run on every built message before signing.
- `bot/src/txbuilder/compute.rs` — Builds ComputeBudget instructions and computes the unit limit from measured CU (sim unitsConsumed) + margin, clamped/floored.
- `bot/src/txbuilder/wsol.rs` — WSOL wrap/unwrap helper: emits createIdempotent->transfer->SyncNative->...->CloseAccount, picks correct token-program id, asserts ATA not frozen.
- `bot/src/txbuilder/token2022.rs` — Token-2022 extension HARD-REJECT filter: unpack each leg mint as PodStateWithExtensions<PodMint>, reject the dangerous extension set, allow plain SPL + fee-only.
- `bot/src/txbuilder/preflight.rs` — simulateTransaction wrapper with the correct flags;
- `bot/src/txbuilder/error.rs` — Error enum for the module.
- `bot/src/txbuilder/config.rs` — Static program IDs, NATIVE_MINT, ATA program, ComputeBudget program, default margin/floor, DEX allowlist references (mirrors onchain allowlist).
- `bot/src/txbuilder/tip.rs` — Jito tip transfer instruction builder (Fase 2 seam, slot reserved day 1).
- `bot/src/txbuilder/tests/limits_tests.rs` — Unit tests for limit validation, byte budgeting, signer-not-in-ALT, loaded-account count.
- `bot/src/txbuilder/tests/token2022_tests.rs` — Table-driven tests for the Token-2022 filter against fixture mint blobs.
- `bot/src/txbuilder/tests/wsol_tests.rs` — Tests WSOL leg emission, ordering, and frozen-ATA assert.
- `infra/alt/mod.rs` — ALT facade re-exported by txbuilder: AltManager, AltHandle, warm-up rules, janitor.
- `infra/alt/manager.rs` — Creates/extends/loads ALTs, tracks per-table address set and slot-added, enforces append-only and ~30/extend chunking, distinguishes static long-lived vs per-route tables…
- `infra/alt/warmup.rs` — Encodes the 1-slot warm-up rule: a key added in slot S is usable only when current_slot > S.
- `infra/alt/janitor.rs` — Async janitor that deactivates and closes stale per-route tables, respecting the ~512-slot SlotHashes window before close reclaims rent.
- `infra/alt/static_set.rs` — Declares the invariant address set for the static long-lived table (programs, sysvars, ATA/system/compute-budget, top mints+ATAs, hot pools+vaults+oracles) and a builder…
- `infra/alt/tests/alt_lifecycle_tests.rs` — Tests warm-up gate, ~30/extend chunking, close-refusal before 512 slots, append-only.
- `infra/alt/config.toml` — ALT operational config: static table pubkey (post-create), close_after_slots, extend_chunk_size, list of invariant addresses.

</details>

**Public interfaces (11):**

```rust
// TxBuilder::build_arb_tx — Assembles the canonical-order v0 VersionedMessage, resolves ALT metas, runs limits::validate(), returns an unsigned BuiltTx { message, limit_report }.
pub fn build_arb_tx(&self, plan: &ArbTxPlan, cu: ComputeBudgetParams, recent_blockhash: Hash) -> Result<BuiltTx, TxBuildError>
// ComputeBudgetParams::from_measured — limit = min(ceil(units_consumed * (10000+margin_bps)/10000), MAX_COMPUTE_UNIT_LIMIT).max(floor).
pub fn from_measured(units_consumed: u64, margin_bps: u16, floor: u32) -> ComputeBudgetParams
// Token2022Filter::vet_mint — Unpacks PodStateWithExtensions<PodMint>;
pub fn vet_mint(&self, mint_account_data: &[u8], owner_program: &Pubkey) -> MintVetVerdict
// WsolDance::wrap_legs — Returns the createIdempotent / SystemProgram.transfer / SyncNative / CloseAccount instructions plus the WSOL ATA pubkey, using the correct token-progr…
pub fn wrap_legs(&self, owner: &Pubkey, lamports: u64) -> WsolLegs
// Preflight::simulate — Calls simulateTransaction with replaceRecentBlockhash:true, sigVerify:false;
pub async fn simulate(&self, rpc: &RpcClient, tx: &VersionedTransaction) -> Result<PreflightResult, TxBuildError>
// Preflight::profit_from_balances — Computes base-asset delta from ACTUAL pre/post token balances (not instruction amount), inclusive of all fees/tip — used to confirm post > pre before…
pub fn profit_from_balances(pre: &[UiTokenAmount], post: &[UiTokenAmount], base_mint: &Pubkey, owner_ata: &Pubkey) -> i128
// limits::validate — Asserts unique account locks <=128, serialized bytes <=1232, loaded accounts <=256, CU limit <=1.4M, and that no signer pubkey appears in any ALT.
pub fn validate(msg: &VersionedMessage, alt_accounts: &[AddressLookupTableAccount], signers: &[Pubkey]) -> Result<LimitReport, TxBuildError>
// AltManager::ensure_keys_present — Determines which keys are already in warm tables vs need extend;
pub async fn ensure_keys_present(&mut self, keys: &[Pubkey], current_slot: u64) -> Result<AltPlan, TxBuildError>
// warmup::is_warm — Returns current_slot > last_extended_slot.
pub fn is_warm(last_extended_slot: u64, current_slot: u64) -> bool
// AltJanitor::try_close — Closes a deactivated table only when current_slot - deactivation_slot > close_after_slots (~512);
pub async fn try_close(&self, handle: &AltHandle, current_slot: u64) -> Result<Option<Signature>, TxBuildError>
// AltManager::extend — Appends up to ~30 addresses (append-only) to a table, updates last_extended_slot.
pub async fn extend(&mut self, handle: &mut AltHandle, chunk: &[Pubkey]) -> Result<Signature, TxBuildError>
```

**Key data structures:**

- **`ArbTxPlan`** — `struct ArbTxPlan { payer: Pubkey, swap1_ix: Instruction, swap2_ix: Instruction, profit_assert_ix: Instruction, wsol: Option<WsolLegs>, tip: Option<TipPlan>, alt…` · Pre-resolved plan handed in by sizing/onchain-account-resolution modules.
- **`BuiltTx`** — `struct BuiltTx { message: VersionedMessage, limit_report: LimitReport, ix_count: u8 }` · Unsigned.
- **`LimitReport`** — `struct LimitReport { account_locks: u16, serialized_bytes: u16, loaded_accounts: u16, cu_limit: u32, headroom_bytes: u16 }` · INVARIANT: account_locks<=128, serialized_bytes<=1232, loaded_accounts<=256, cu_limit<=1_400_000.
- **`ComputeBudgetParams`** — `struct ComputeBudgetParams { unit_limit: u32, unit_price_micro_lamports: u64 }` · unit_limit derived from sim unitsConsumed + margin, clamped to 1.4M, floored.
- **`WsolLegs`** — `struct WsolLegs { create_idempotent: Instruction, fund_lamports: Instruction, sync_native: Instruction, close: Instruction, wsol_ata: Pubkey, token_program: Pub…` · INVARIANT: create_idempotent emitted before any swap reading the WSOL ATA;
- **`MintVetVerdict`** — `enum MintVetVerdict { Allow { is_token2022: bool, has_transfer_fee: bool }, Reject(RejectReason) }  enum RejectReason { TransferHook, NonTransferable, DefaultAc…` · HARD-REJECT set per §9.
- **`AltHandle`** — `struct AltHandle { address: Pubkey, addresses: Vec<Pubkey>, last_extended_slot: u64, deactivation_slot: Option<u64>, kind: AltKind }  enum AltKind { StaticLongL…` · append-only addresses (no per-address delete).
- **`PreflightResult`** — `struct PreflightResult { err: Option<String>, units_consumed: u64, fee: u64, pre_token_balances: Vec<UiTokenBalance>, post_token_balances: Vec<UiTokenBalance>,…` · From RpcSimulateTransactionResult.
- **`TipPlan`** — `struct TipPlan { tip_account: Pubkey, lamports: u64 }` · tip_account resolved at runtime via getTipAccounts (8 accounts, load-balanced);

**External crates:** `solana-sdk / solana-program (native, pinned to the Agave 3.x line the workspace targets — NOT Anchor on hot path)`, `solana-program-runtime / solana-compute-budget (ComputeBudget program ix builders)`, `solana-address-lookup-table-program (create/extend/deactivate/close ix + AddressLookupTableAccount)`, `solana-rpc-client / solana-rpc-client-api (simulateTransaction with RpcSimulateTransactionConfig: replaceRecentBlockhash, sigVerify, accounts)`, `spl-associated-token-account (createIdempotent for WSOL ATA)`, `spl-token + spl-token-2022 (NATIVE_MINT, SyncNative, CloseAccount; PodStateWithExtensions<PodMint> + extension getters for the filter)`, `spl-pod (Pod mint/extension decoding)`, `tokio (async ALT manager / janitor / preflight RPC)`, `serde + toml (config)`, `thiserror (error enum)`

**Grounded in `plan.md`:** §3 (architecture / tx builder layer ordering + budget); §6 (CPI cost runtime-read, hard limits, ALT lifecycle, profit-assert correctness, simulateTransaction flags + return fields); §9 (Token-2022 HARD-REJECT filter, WSOL dance, pool/mint vetting); §1-2 (atomic single-tx all-or-nothing, tip-inside-tx => fail=tip-unpaid, no mempool); §11 (phase plan Fase 0-4)

**Tasks (14):**

- **`txbuilder-1` Module scaffold, config, hard-limit constants** · Fase 0 · 1.5d · deps: —
  - Create bot/src/txbuilder/ + infra/alt/ skeletons, config.rs/config.toml with verified program IDs (Raydium CPMM CPMMoo8L.., Orca Whirlpool whirLb.., PumpSwap pAMMBay.., NATIVE_MINT, ATA/system/compute-budget programs), and limits.rs with MAX_TX_ACCOUNT_LOCKS=128 / MAX_TX_BYTES=1232 / MAX_COMPUTE_UNIT_LIMIT=1_400_000 /…
  - **Done when:** cargo build green; all program IDs asserted to decode as valid base58 pubkeys in a unit test · limits constants exactly match §6 (128 not 256; 1232; 1.4M; 256 loaded) · deps pinned + Cargo.lock committed
- **`txbuilder-2` ComputeBudget instruction builder + measured-CU sizing** · Fase 0 · 1d · deps: `txbuilder-1`
  - Implement compute.rs: set_cu_limit_ix / set_cu_price_ix and ComputeBudgetParams::from_measured(units_consumed, margin_bps, floor) clamping to 1.4M and flooring at a configurable minimum. unit_price supplied externally by fee-strategy.
  - **Done when:** from_measured(1_000_000, 1000, _) == 1_100_000 · from_measured(1_400_000, 1000, _) clamps to 1_400_000 · floor enforced when units tiny · over-request never exceeds 1.4M cap
- **`txbuilder-3` Token-2022 HARD-REJECT extension filter** · Fase 1 · 2.5d · deps: `txbuilder-1`
  - Implement token2022.rs vet_mint() unpacking PodStateWithExtensions<PodMint>; reject TransferHook (incl null-program under default config), NonTransferable, DefaultAccountState=frozen, MemoTransfer, ConfidentialTransfer, PermanentDelegate, MintCloseAuthority; allow plain SPL + fee-only Token-2022; allow InterestBearing/…
  - **Done when:** each dangerous extension produces its RejectReason against a real fixture mint blob · fee-only Token-2022 => Allow{has_transfer_fee:true} · plain SPL (Token program owner) => Allow · null-program TransferHook rejected when allow_null_transfer_hook=false (documented as discarding safe mints) · InterestBearing/ScaledUiAmount => Allow
- **`txbuilder-4` WSOL dance helper** · Fase 1 · 1.5d · deps: `txbuilder-1`
  - Implement wsol.rs: createIdempotent -> SystemProgram.transfer lamports -> SyncNative -> CloseAccount, returning WsolLegs with correct token-program id (shared ATA program). Add assert_not_frozen before use. Provide ordering guarantees consumed by builder (create before swap, close after swaps).
  - **Done when:** wrap_legs emits all four ixs with NATIVE_MINT So111..112 · frozen ATA => WsolAtaFrozen error · close always present (no stuck rent/WSOL) · correct token program selected for Token vs Token-2022 WSOL
- **`txbuilder-5` Canonical instruction layout + core message assembler** · Fase 1 · 2.5d · deps: `txbuilder-2`, `txbuilder-4`, `txbuilder-9`
  - Implement layout.rs (IxSlot enum + ORDER) and builder.rs build_arb_tx(): emit [cu_limit, cu_price, (wsol_create,fund,sync), swap1, swap2, profit_assert, (wsol_close), (tip)] in canonical order skipping disabled optional slots, resolve ALT metas, compile_to_v0_message. Reserve the tip slot from day 1 even when tip disab…
  - **Done when:** instruction order matches §3 exactly · optional WSOL/tip slots correctly skipped when absent · v0 message compiles with ALT account metas · returns unsigned BuiltTx (no signing in this module)
- **`txbuilder-6` Hard-limit validation gate** · Fase 1 · 2d · deps: `txbuilder-5`
  - Implement limits.rs validate(): count unique writable+readonly locks across static + ALT-resolved keys, serialized byte length, loaded-account count, CU<=1.4M, and assert no signer key lives in any referenced ALT. Wire into build_arb_tx as a mandatory post-assembly gate returning LimitReport.
  - **Done when:** full 2-swap+WSOL+tip message validated < 1232 bytes and < 128 locks · signer present in an ALT => SignerInAlt error · loaded accounts > 256 => error · CU limit > 1.4M => error · LimitReport emitted for metrics
- **`txbuilder-7` Preflight simulateTransaction wrapper + profit-check** · Fase 1 · 2.5d · deps: `txbuilder-5`, `txbuilder-2`
  - Implement preflight.rs simulate() with RpcSimulateTransactionConfig{replaceRecentBlockhash:true, sigVerify:false}; parse err, unitsConsumed, fee, pre/postTokenBalances, replacementBlockhash, loadedAccountsDataSize, logs. Implement profit_from_balances reading ACTUAL base-asset delta. Feed unitsConsumed into ComputeBudg…
  - **Done when:** sim called with replaceRecentBlockhash:true + sigVerify:false (never both flags) · profit computed from pre/postTokenBalances, not instruction amount · unitsConsumed wired to CU limit · preflight is advisory only — documented that on-chain assert is sole safety net even with skipPreflight=true
- **`txbuilder-8` ALT manager: create/extend/resolve with append-only + ~30 chunking** · Fase 1 · 2.5d · deps: `txbuilder-1`
  - Implement infra/alt/manager.rs: create(), extend(chunk<=30, append-only), resolve_metas(), ensure_keys_present(); track last_extended_slot and addresses per AltHandle; distinguish StaticLongLived vs PerRoute.
  - **Done when:** extend rejects chunks > ~30 · filling 256 needs ~9 extends · resolve_metas returns AddressLookupTableAccount list for v0 compile · addresses append-only (no per-address delete)
- **`txbuilder-9` ALT warm-up gate + static-table set** · Fase 1 · 2d · deps: `txbuilder-8`
  - Implement warmup.rs is_warm/assert_warm_or_err/wait_until_warm and static_set.rs invariant_addresses + build_static_table_plan (chunks of <=30). Wire warm-up assertion into ensure_keys_present so a freshly-extended key is never used in the same slot. Pre-warm the static long-lived table (programs/sysvars/ATA/compute-bu…
  - **Done when:** is_warm false at last_extended_slot, true at +1 · build_arb_tx refuses to reference a not-yet-warm key (AltNotWarm) · static table plan chunks <=30 · NEVER extend-then-use same slot enforced by code path, not convention
- **`txbuilder-10` ALT janitor (async close after ~512 slots)** · Fase 1 · 1.5d · deps: `txbuilder-8`
  - Implement infra/alt/janitor.rs: deactivate() then try_close() that refuses until current_slot - deactivation_slot > 512 (SlotHashes window), run as an async loop, never synchronous in the hot path. Targets per-route tables for reclaim.
  - **Done when:** try_close returns Ok(None) before 512 slots elapsed · close issued and rent reclaimed after window · janitor runs off the hot path (async task)
- **`txbuilder-11` Pre-build route vetting integration (filter + DEX allowlist mirror + frozen-ATA)** · Fase 1 · 1.5d · deps: `txbuilder-3`, `txbuilder-4`
  - Compose token2022 filter + WSOL frozen-ATA assert + a mirror of the onchain DEX allowlist into a single vet_route() guard run BEFORE assembling a tx, so poison routes burn no fee/tip/CU. Reject if any leg mint is rejected or any swap-CPI target is not in the allowlist (CPMMoo8L../whirLb../pAMMBay..).
  - **Done when:** non-allowlisted swap-CPI program => route rejected pre-build · rejected mint => route rejected pre-build · balance-read accounts asserted owned by bot authority (mirrors onchain trust boundary)
- **`txbuilder-12` End-to-end build harness on mainnet-fork (LiteSVM/Surfpool)** · Fase 1 · 2.5d · deps: `txbuilder-6`, `txbuilder-7`, `txbuilder-9`
  - Wire builder + ALT + preflight against cloned Raydium CPMM + Orca Whirlpool pools: build a full [cu, wsol, swap1, swap2, assert] tx, pre-warm a real ALT, simulate, and confirm limit_report (locks<128, bytes<1232, CU<1.4M) and profit-check parity with on-chain realized. This satisfies the Fase-1 checklist items for the…
  - **Done when:** CU per leg measured; total < 1.4M; locks < 128; bytes < 1232 · ALT pre-warmed >=1 slot before use; no same-slot extend-then-use · WSOL wrap->sync->close complete, no stuck ATA · sim profit-check matches on-chain realized delta both directions
- **`txbuilder-13` Jito tip instruction (Fase 2 seam) + tip capping** · Fase 2 · 1.5d · deps: `txbuilder-5`
  - Implement tip.rs: tip_transfer_ix (system transfer to a getTipAccounts-resolved account, INSIDE the atomic tx), cap_tip(simulated_profit, fraction_bps, floor=1000). Activate the reserved tip slot in the layout. tip_account resolution + load-balancing across 8 accounts owned by landing/executor; this module only consume…
  - **Done when:** tip transfer is the last instruction and inside the arb tx (fail => tip unpaid) · tip lamports >= 1000 and <= configured fraction of simulated profit · tip_account never hardcoded (passed in from runtime getTipAccounts resolution)
- **`txbuilder-14` PumpSwap AMM venue support in builder/vet (Fase 2)** · Fase 2 · 1.5d · deps: `txbuilder-11`
  - Extend config DEX allowlist and vet_route to accept PumpSwap AMM (pAMMBay..) as a third constant-product venue; ensure account-meta resolution and limit budgeting hold for PumpSwap legs. Swap-ix construction convention comes from onchain module.
  - **Done when:** PumpSwap pAMMBay.. accepted by allowlist mirror · a CPMM+PumpSwap or Whirlpool+PumpSwap route builds within locks/bytes/CU limits

<details><summary><b>Open questions</b> (6)</summary>

- TransferHook nuance: default is blanket HARD-REJECT (allow_null_transfer_hook=false), discarding genuinely-safe null-program-hook mints like PYUSD. Do we want a Fase 3 toggle to admit null-hook mints to capture that volume, and who owns the per-mint allowlist?
- Static-ALT churn policy: when a new hot pool appears, do we always extend the single static long-lived table (accepting 1-slot warm-up before first use) or spin a per-route table off-path? Threshold (e.g., expected reuse count) is unspecified.
- CU margin default: spec says measured+~10%; is a flat 10% adequate across venues, or should margin be per-venue/per-leg-count given PumpSwap/Whirlpool CPI cost differences (and post-SIMD-0339 CU/CPI changes read at runtime)?
- Who owns getTipAccounts resolution and the 8-account load-balancer — landing/executor (assumed) — and exactly what struct does it hand the txbuilder tip module?
- Per-route ALT necessity: for Milestone-1 ~20-50 targeted pairs, can everything fit in one static long-lived table (<=256 addresses), making per-route tables + janitor effectively dormant until Fase 3?
- Exact serialized-byte accounting for ALT-resolved keys in limits::validate — need to confirm the v0 message-size formula counts lookup-table index bytes (1/key) plus the table address (32) so the 1232 gate is exact, not conservative.

</details>

---

### 5.6 `landing` — Landing / Executor — Jito bundles, tip sizing, Helius Sender, landing loop

**Directory:** `bot/src/executor/`

**Purpose.** Owns the final mile of the atomic-arb hot path: taking a signed-but-not-yet-submitted (or sign-on-demand) atomic arb VersionedTransaction and getting it LANDED on mainnet profitably. Responsibilities: (1) assemble the Jito 1-tx bundle with the tip transfer placed INSIDE the same atomic tx and the jitodontfront stamp; (2) resolve and load-balance the 8 Jito tip accounts at runtime via getTipAccounts (never hardcoded); (3) size the tip from tip_floor REST + tip_stream WS at the 50th-75th baseline, capped as a fraction of simulated profit; (4) validate via simulateBundle / simulateTransaction (replaceRecentBlockhash:true + sigVerify:false) reading pre/postTokenBalances BEFORE paying any tip; (5…

<details><summary><b>File map</b> (16 files)</summary>

- `bot/src/executor/mod.rs` — Module root.
- `bot/src/executor/facade.rs` — Top-level Executor that the strategy/orchestrator calls with a built+signed arb tx (or a tx template + sign closure).
- `bot/src/executor/bundle_build.rs` — Builds the single atomic v0 VersionedTransaction: arb instructions + ComputeBudget (SetComputeUnitLimit/Price) + the Jito tip SystemProgram::transfer + the jitodontfront…
- `bot/src/executor/jito.rs` — Jito Block Engine JSON-RPC client: getTipAccounts (cached w/ TTL), sendBundle, getInflightBundleStatuses, getBundleStatuses, simulateBundle.
- `bot/src/executor/regions.rs` — Region enum + endpoint table for the 8 Block Engine regions, latency-probe ranking to pick nearest, and the fan-out set selection.
- `bot/src/executor/tip.rs` — TipOracle: maintains live tip_floor (REST poll of bundles.jito.wtf percentiles 25/50/75/95/99 + ema) and tip_stream (WS).
- `bot/src/executor/sim.rs` — Pre-tip simulation gate.
- `bot/src/executor/landing_loop.rs` — The strict landing loop state machine: submit -> poll getInflightBundleStatuses (window ~5min) -> getBundleStatuses (~300 slot).
- `bot/src/executor/sender.rs` — Helius Sender fallback client (sender.helius-rpc.com/fast).
- `bot/src/executor/swqos.rs` — Non-bundle SWQoS sendTransaction path for Frankendancer/non-bundle slots: routes through a staked connection (staked RPC/validator TPU), sets priority fee from the data-m…
- `bot/src/executor/metrics.rs` — First-class health metrics: revert-rate, burn-rate (lamports/min on reverted losers), submit/land latency P50/P95, confirmation rank, drop-cause histogram.
- `bot/src/executor/types.rs` — Shared data types: ArbTxSpec (the rebuildable description of an arb tx), DropCause enum, LandingOutcome, Route, BundleId, BundleStatus, InflightStatus, TipFloorSnapshot.
- `bot/src/executor/config.rs` — Typed config loaded from infra config (TOML): regional endpoints, allowlisted UUID, tip percentile band + profit-fraction cap, no-land timeout, max attempts, sender mode…
- `bot/src/executor/config/executor.toml` — Default config values for Fase 2: region order, percentile band 50-75, tip_profit_cap_frac, no_land_ms=2500, inflight_window_s=300, sender default swqos_only, durable_non…
- `bot/src/executor/tests/landing_loop_tests.rs` — Unit tests for the loop state machine with a mock JitoClient: asserts fresh-blockhash rebuild on no-land, no blockhash reuse, deadline/max-attempt termination, drop-cause…
- `bot/src/executor/tests/tip_sizing_tests.rs` — Property tests for tip sizing: tip in [50th,75th] band when profit allows, never exceeds profit_cap_frac*profit, never below MIN_TIP_LAMPORTS, round-robin covers all 8 ac…

</details>

**Public interfaces (12):**

```rust
// Executor::land — Primary entry point called by the strategy orchestrator with the arb instruction set, ALT, simulated profit, and CU budget.
pub async fn land(&self, req: LandRequest) -> Result<LandingOutcome, LandingError>
// SignerHandle (consumed trait, owned by key-mgmt module) — The executor depends on (does NOT implement) the signer sidecar.
#[async_trait] pub trait SignerHandle: Send + Sync { fn signing_enabled(&self) -> bool; async fn sign_arb_tx(&self, msg: &VersionedMessage, shape: &TxShapeClaim) -> Result<Signature, SignerError>; fn payer(&self) -> Pubkey; }
// JitoClient::send_bundle — Submits a (1-tx) bundle to a regional Block Engine over JSON-RPC with x-jito-auth UUID, subject to the per-region 1 req/s token bucket.
pub async fn send_bundle(&self, region: Region, txs: &[VersionedTransaction]) -> Result<BundleId, JitoError>
// JitoClient::tip_accounts — Runtime resolution of the 8 tip accounts via getTipAccounts, cached with a TTL.
pub async fn tip_accounts(&self) -> Result<[Pubkey; 8], JitoError>
// JitoClient::simulate_bundle — Calls Jito-Solana simulateBundle (Triton/Helius/QuickNode lil'JIT) to validate the bundle before paying the tip.
pub async fn simulate_bundle(&self, region: Region, txs: &[VersionedTransaction]) -> Result<SimBundleResult, JitoError>
// TipOracle::size_tip — Computes the tip from the live tip_floor band (50th-75th baseline scaled by tip-per-CU competition), then caps at profit_cap_frac * sim_profit_lamport…
pub fn size_tip(&self, sim_profit_lamports: u64, cu: u32) -> TipDecision
// TipOracle::next_tip_account — Load-balances across the 8 runtime-resolved tip accounts (LRU/round-robin) to spread auction load.
pub fn next_tip_account(&self) -> Pubkey
// simulate_for_profit — Runs simulateTransaction with replaceRecentBlockhash:true + sigVerify:false (mutually exclusive flags honored), reads pre/postTokenBalances + fee + un…
pub async fn simulate_for_profit(rpc: &RpcClient, tx: &VersionedTransaction) -> Result<SimProfit, SimError>
// run_landing_loop — The submit->inflight->status loop.
pub async fn run_landing_loop(deps: &LoopDeps, spec: ArbTxSpec) -> LandingOutcome
// HeliusSender::send_fast — Helius Sender /fast fallback: enforces skipPreflight=true + maxRetries=0, requires tip transfer + setComputeUnitPrice present, mode swqos_only|dual wi…
pub async fn send_fast(&self, tx: &VersionedTransaction, mode: SenderMode) -> Result<Signature, SenderError>
// SwqosSender::send_staked — Non-bundle staked-connection sendTransaction for non-bundle (Frankendancer) slots.
pub async fn send_staked(&self, tx: &VersionedTransaction, prio_micro: u64) -> Result<Signature, SwqosError>
// ExecutorMetrics::record_outcome — Updates revert-rate, burn-rate, latency P50/P95, confirmation rank, and the drop-cause histogram;
pub fn record_outcome(&self, outcome: &LandingOutcome)
```

**Key data structures:**

- **`ArbTxSpec`** — `struct ArbTxSpec { arb_ixs: Vec<Instruction>, alt: Vec<AddressLookupTableAccount>, payer: Pubkey, cu_limit: u32, cu_price_micro: u64, sim_profit_lamports: u64,…` · The REBUILDABLE description of one arb attempt.
- **`TipFloorSnapshot`** — `struct TipFloorSnapshot { p25: u64, p50: u64, p75: u64, p95: u64, p99: u64, ema: u64, ts: Instant }` · Latest tip_floor from REST/WS in lamports.
- **`TipDecision`** — `struct TipDecision { lamports: u64, percentile_used: f32, capped_by_profit: bool, account: Pubkey }` · Invariants: MIN_TIP_LAMPORTS(1000) <= lamports;
- **`BundleId`** — `struct BundleId([u8;32]) // SHA-256 of tx signatures` · Receipt only — NOT a landing guarantee.
- **`InflightStatus`** — `enum InflightStatus { Invalid, Pending, Failed, Landed, NotFound }` · From getInflightBundleStatuses (~5min window).
- **`BundleStatus`** — `struct BundleStatus { id: BundleId, confirmation: ConfStatus, slot: Option<u64>, txs: Vec<Signature>, err: Option<String> } enum ConfStatus { Processed, Confirm…` · From getBundleStatuses (~300 slot window).
- **`DropCause`** — `enum DropCause { TipAuctionLost, Congestion, TooLateInSlot, SimFailed, StaleBlockhash, UncledOrSkipped, SenderRejected, RateLimited, Unknown }` · Co-dominant per §6 — the loop attributes a best-effort cause per attempt;
- **`LandingOutcome`** — `enum LandingOutcome { Landed { slot: u64, sig: Signature, attempts: u8, tip_paid_lamports: u64, route: Route, latency: Duration }, Reverted { sig: Signature, fe…` · tip_paid_lamports>0 only on Landed (tip inside atomic tx => reverted/gaveup pay base+priority only).
- **`Route`** — `enum Route { JitoBundle { region: Region }, HeliusSender { mode: SenderMode }, Swqos }` · Routing-exclusivity invariant: a tx carrying a jitodontfront marker must NEVER be emitted via HeliusSender/Swqos paths that lack Block-Engine protection;
- **`TxShapeClaim`** — `struct TxShapeClaim { tip_dest: Pubkey, max_lamport_out: u64, allowlisted_programs: Vec<Pubkey>, has_jitodontfront: bool }` · Handshake passed to the signer sidecar so its synchronous pre-sign cap + tx-shape validation can approve the executor-injected tip transfer and jitodontfront account.
- **`RegionRateLimiter`** — `struct RegionRateLimiter { buckets: HashMap<Region, TokenBucket> } // 1 token/s/region` · Enforces Jito default 1 req/s/IP/region;

**External crates:** `solana-sdk / solana-program (Agave) — VersionedTransaction, v0::Message, AddressLookupTableAccount, ComputeBudgetInstruction, system_instruction::transfer, Hash, Pubkey, Signature`, `solana-client / solana-rpc-client (Agave) — RpcClient, simulateTransaction with RpcSimulateTransactionConfig{replace_recent_blockhash:true, sig_verify:false}`, `tokio (async runtime), reqwest (Jito REST tip_floor + Block Engine JSON-RPC + Helius Sender), tokio-tungstenite (tip_stream WS)`, `serde / serde_json (JSON-RPC payloads), sha2 (bundle_id verification), governor or custom token-bucket (per-region 1 req/s rate limit)`, `tracing + metrics/prometheus (observability), proptest (tip-sizing + loop property tests), thiserror (error enums)`, `NOTE: hot path is native Rust per shared invariant; no Anchor. TS only appears in the data/tooling modules, not here.`

**Grounded in `plan.md`:** §1 (atomic-first, success criteria #3 tip-inside-atomic); §2 (revert cost, tip-not-paid-on-fail, no mempool); §6 (Jito bundles/tips/Helius Sender/simulate flags); §8 (landing vs data separation, SWQoS); §9 (jitodontfront, cap tip as profit fraction, revert-rate, signer tx-shape); §11 Fase 0/1/2

**Tasks (10):**

- **`landing-1` Jito account, UUID and Sender baseline (Fase 0 setup seam)** · Fase 0 · 2d · deps: —
  - Obtain the Jito allowlisted UUID (x-jito-auth) for bundle rate-limit, verify getTipAccounts resolves the 8 accounts at runtime, register the Helius Sender (free, 0 credits) endpoint, and pin all landing-related endpoints/program ids in infra config. Stand up throwaway hot keypair (chmod 600, not in git, deps pinned + l…
  - **Done when:** getTipAccounts returns 8 accounts at runtime and is NOT hardcoded · Jito allowlisted UUID present and used as x-jito-auth header · ExecutorConfig rejects tip band outside 50-75 and profit_cap_frac outside (0,1) at load · Helius Sender endpoint reachable
- **`landing-2` JitoClient JSON-RPC + regional fan-out + rate limiter** · Fase 2 · 4d · deps: `landing-1`
  - Implement jito.rs and regions.rs: getTipAccounts (TTL cache), sendBundle, getInflightBundleStatuses, getBundleStatuses, simulateBundle over JSON-RPC with x-jito-auth. Add the 8-region endpoint table, latency-probe ranking, and a per-region 1 req/s token-bucket. Implement fan_out submitting to the nearest region plus N…
  - **Done when:** sendBundle returns bundle_id and code never treats it as confirmation · per-region requests capped at 1 req/s (token bucket test) · fan_out submits to nearest region first, ranked by measured latency · getInflightBundleStatuses and getBundleStatuses parsed into typed enums
- **`landing-3` TipOracle: tip_floor REST + tip_stream WS, sizing + load-balance** · Fase 2 · 3d · deps: `landing-2`
  - Implement tip.rs: poll bundles.jito.wtf tip_floor (percentiles + ema) over REST and subscribe tip_stream over WS. size_tip computes a 50th-75th baseline scaled by tip-per-CU competition, then caps at profit_cap_frac*sim_profit and clamps to >=1000 lamports. Load-balance next_tip_account across the 8 runtime-resolved ac…
  - **Done when:** tip never exceeds profit_cap_frac*sim_profit (property test) · tip never below 1000 lamports · tip sits in [p50,p75] band when profit allows · all 8 tip accounts exercised by the load balancer · stale tip_floor (>max_age) triggers conservative fallback
- **`landing-4` Bundle build: tip-inside-atomic-tx + jitodontfront + hard-limit guard** · Fase 2 · 4d · deps: `landing-3`
  - Implement bundle_build.rs: take the txbuilder arb instructions, inject SetComputeUnitLimit/Price, the Jito tip SystemProgram::transfer to a resolved tip account, and the jitodontfront read-only non-signer account; compile to v0 against the pre-warmed ALT. Enforce the hard limits (128 account locks, 1232 bytes, 1.4M CU,…
  - **Done when:** compiled tx contains the tip transfer and a jitodontfront-prefixed read-only non-signer account · assert_tx_limits rejects >128 locks / >1232 bytes / >1.4M CU / >256 loaded accounts before signing · TxShapeClaim correctly declares tip_dest + max_lamport_out(tip+fee) · revert path proven to NOT pay tip (LiteSVM/sim cross-check) · signers are never placed in the ALT
- **`landing-5` Pre-tip simulation gate (simulateTransaction / simulateBundle)** · Fase 2 · 3d · deps: `landing-4`
  - Implement sim.rs: run simulateTransaction with replaceRecentBlockhash:true + sigVerify:false (mutually exclusive honored), parse pre/postTokenBalances + fee + unitsConsumed + err, compute realized sim-profit from actual balance delta, and gate tip payment on a positive simulated profit. Also wire Jito simulateBundle be…
  - **Done when:** sim uses replaceRecentBlockhash:true + sigVerify:false (never both verify+replace) · profit computed from pre/postTokenBalances delta, not instruction amount · tip is paid only after simulateBundle passes · sim failure attributed as DropCause::SimFailed, never relied on for fee protection
- **`landing-6` Strict landing loop with fresh-blockhash rebuild** · Fase 2 · 4d · deps: `landing-5`
  - Implement landing_loop.rs: submit -> poll getInflightBundleStatuses (~5min) -> getBundleStatuses (~300 slot). On ~2-3s no-land, REBUILD ArbTxSpec with a fresh blockhash (re-sim, re-size tip via TipOracle, re-sign), never reusing a blockhash; bound by deadline and max_attempts. Attribute each failed attempt to a DropCau…
  - **Done when:** no blockhash is ever reused across attempts (test asserts distinct blockhashes) · no-land after ~2.5s triggers a full rebuild · loop terminates on deadline/max_attempts with GaveUp + last DropCause · Landed cross-checked via getBundleStatuses (slot+sig) not just inflight · durable-nonce remains behind a disabled feature flag
- **`landing-7` Helius Sender fallback + SWQoS non-bundle path + routing-exclusivity guard** · Fase 2 · 3d · deps: `landing-6`
  - Implement sender.rs and swqos.rs: Helius Sender /fast with skipPreflight=true + maxRetries=0 (self-retry in the loop), swqos_only|dual modes with their min tips, requiring tip transfer + setComputeUnitPrice. Implement the staked-connection SWQoS sendTransaction for non-bundle slots using the data-module priority-fee es…
  - **Done when:** Helius Sender call always sets skipPreflight=true + maxRetries=0 · swqos_only enforces 0.000005 SOL min tip, dual enforces 0.0002 SOL · routing-exclusivity guard rejects sending a jitodontfront tx via Sender/SWQoS · SWQoS path uses a staked connection + priority fee from the data module
- **`landing-8` Executor facade, route selection, signer handshake** · Fase 2 · 3d · deps: `landing-7`
  - Implement facade.rs: Executor::land orchestrating sim-gate -> tip-size -> bundle-build (+ TxShapeClaim) -> signer.sign_arb_tx (after signing_enabled check) -> route selection (Jito primary, Sender/SWQoS fallback) -> landing loop. Default route = Jito bundle; fallback only when routing-exclusivity permits. Returns Landi…
  - **Done when:** signing_enabled (kill-switch) checked before every sign · default route is Jito bundle; fallback respects routing-exclusivity · LandingOutcome::Landed carries slot+sig+tip_paid+route+latency · demonstrates >=1 profitable mainnet land at small size
- **`landing-9` Executor metrics: revert-rate, burn-rate, latency, drop-cause** · Fase 2 · 2d · deps: `landing-8`
  - Implement metrics.rs: record revert-rate (with >30% infra-bug alert), burn-rate (lamports/min on reverted losers), submit/land latency P50/P95, confirmation rank, and a drop-cause histogram. Expose a HealthSnapshot consumed by the kill-switch supervisor for auto-trip and by the dashboard.
  - **Done when:** revert-rate and burn-rate are first-class gauges · revert-rate >30% raises an infra-bug alert signal · drop-cause histogram populated from landing-loop attributions · HealthSnapshot consumable by supervisor auto-trip
- **`landing-10` Durable-nonce forward seam (Fase 3 hook, design-only for M1)** · Fase 3 · 1d · deps: `landing-6`
  - FORWARD HOOK (forces a day-1 design seam): keep ArbTxSpec and the landing loop blockhash-source abstracted so a durable-nonce evaluation can replace fresh-blockhash rebuild later without restructuring. Build the trait/enum seam now; leave the durable-nonce implementation feature-flagged OFF for Milestone 1. Documented…
  - **Done when:** landing loop reads blockhash via a BlockhashSource seam, not a hardcoded RPC call · durable-nonce variant compiles but is disabled by default · no Milestone-1 behavior change

<details><summary><b>Open questions</b> (8)</summary>

- Exact profit_cap_frac value: spec says 'cap tip as fraction of profit' (cites tip wars eating ~50-70% on saturated pairs) but gives no number. Default proposed 0.5 — needs live tuning per venue/competition.
- Tip-per-CU scaling curve within the 50th-75th band: how aggressively to lerp toward p75 as competition rises is unspecified; needs empirical calibration from drop-cause TipAuctionLost rate.
- no-land rebuild threshold: spec says ~2-3s; chosen 2500ms — should this adapt to measured slot timing / region latency?
- Fan-out width N (how many backup regions beyond nearest) vs the 1 req/s/region limit and duplicate-bundle cost — needs a cost/land-rate experiment.
- Default Helius Sender mode (swqos_only vs dual): swqos_only is cheaper (0.000005 SOL) but dual lands more reliably; default chosen swqos_only — revisit once revert/land metrics exist.
- Whether a jitodontfront tx may EVER use a non-Jito fallback: current design hard-rejects it (routing-exclusivity). If profitable-tx copy risk is deemed low for some venues, an opt-out could be added — left closed for M1.
- Durable-nonce account provisioning/ownership (which key, rent) if/when the Fase 3 seam is activated.
- Burn-rate denominator window (per-minute vs per-slot) and whether to bill reverted-loser priority fees against PnL in real time vs batch.

</details>

---

### 5.7 `signer` — Signer sidecar — key mgmt, synchronous pre-sign caps, kill-switch, sweeper, deploy/ops

**Directory:** `bot/src/signer/ + ops/`

**Purpose.** In-process ed25519 signing sidecar that is the SOLE outflow gate of the bot. It holds only a small-balance hot key (Solana Keychain SolanaSigner, Memory backend), and before every signature it enforces (1) a kill-switch `signing-enabled` flag, (2) tx-shape validation against the arb template (allowlisted program ids, dest == own ATA, max-lamport-out cap), and (3) SYNCHRONOUS pre-sign caps (per-interval signature count + cumulative lamport-out drawn from a local balance snapshot, no RPC round-trip) so worst-case outflow per window is bounded before lagging revert-rate/loss metrics catch up. A kill-switch supervisor consumes health metrics (revert-rate, realized-loss, balance-deviation, burn-r…

<details><summary><b>File map</b> (24 files)</summary>

- `bot/src/signer/mod.rs` — Module root: re-exports public surface (SignerSidecar, SignGuard, KillSwitch, SweeperHandle, signer error type) and wires submodules.
- `bot/src/signer/sidecar.rs` — SignerSidecar: owns the SolanaSigner (Memory/hot backend), the kill-switch flag handle, the PreSignCaps, and the TxShapeValidator.
- `bot/src/signer/keychain.rs` — SolanaSigner trait + backends.
- `bot/src/signer/validate.rs` — TxShapeValidator: parses a compiled v0 VersionedMessage and rejects anything not matching the arb template.
- `bot/src/signer/caps.rs` — PreSignCaps: the SYNCHRONOUS local rate/outflow ceiling (no RPC).
- `bot/src/signer/killswitch.rs` — KillSwitch supervisor + handle.
- `bot/src/signer/thresholds.rs` — KillThresholds struct with pinned numeric defaults + serde load from ops/config/killswitch.toml.
- `bot/src/signer/sweeper.rs` — Blast-radius sweeper.
- `bot/src/signer/health.rs` — HealthSnapshot + ingestion.
- `bot/src/signer/alert.rs` — AlertSink trait + Telegram and PagerDuty implementations + a stdout/log fallback sink.
- `bot/src/signer/error.rs` — SignerError enum: the single error type returned by the sign path.
- `bot/src/signer/metrics.rs` — SignerMetrics: prometheus-style counters/gauges for the signer surface (signatures_total, shape_rejections_total{reason}, cap_exceeded_total{kind}, halt_trips_total{reaso…
- `bot/src/signer/config.rs` — Typed config loader for the whole signer module: paths to hot keypair, treasury pubkey, allowlist program ids (CPMM/Whirlpool/PumpSwap + system/token/ATA/compute-budget),…
- `bot/src/signer/tests/shape_reject.rs` — Unit tests for TxShapeValidator: reject CPI to non-allowlisted program;
- `bot/src/signer/tests/caps_killswitch.rs` — Unit tests for synchronous caps + kill-switch: count cap blocks the (N+1)th sign in a window;
- `ops/config/signer.toml` — Canonical signer runtime config (loaded by config.rs).
- `ops/config/killswitch.toml` — Numeric kill-switch thresholds + alert routing + on-call posture.
- `ops/deny.toml` — cargo-deny config for supply-chain hygiene: ban yanked crates, enforce advisory-db checks (RUSTSEC), license allowlist, and source allowlist (crates.io + pinned git rev o…
- `ops/scripts/verify_build.sh` — Reproducible-build verification: runs `solana-verify build` + `solana-verify verify-from-repo` against the deployed program id so on-chain bytecode matches source.
- `ops/scripts/deploy_squads.sh` — Deploy/upgrade script that sets the program upgrade authority to the Squads multisig and routes upgrades through a Squads proposal (buffer write -> propose -> multisig ap…
- `ops/scripts/rotate_hot_key.sh` — Hot-key rotation: generate new chmod-600 keypair, fund minimal working capital from treasury (Squads-approved), atomically swap signer config + restart sidecar, sweep res…
- `ops/runbooks/killswitch_recovery.md` — Written recovery runbook (the spec demands this explicitly).
- `ops/runbooks/deploy_upgrade.md` — Program upgrade runbook: Squads proposal flow, solana-verify gate before execute, ALT/account-lock regression check, rollback plan, and the immutable-after-stable decisio…
- `ops/SUPPLY_CHAIN.md` — Supply-chain hygiene policy doc separating the two threat models per §9 (dependency-malware vs opsec/treasury).

</details>

**Public interfaces (12):**

```rust
// SignerSidecar::sign_arb_tx — THE single hot-path entry that touches the hot key.
pub fn sign_arb_tx(&self, msg: &solana_sdk::message::VersionedMessage, ctx: &ArbSignContext) -> Result<solana_sdk::signature::Signature, SignerError>
// ArbSignContext — Side-channel the caller passes so the validator can resolve ALT-referenced keys (signers are never in ALT — asserted) and so caps can charge the corre…
pub struct ArbSignContext { pub loaded_addresses: solana_sdk::message::v0::LoadedAddresses, pub expected_lamport_out: u64, pub tip_lamports: u64, pub route_pool_pubkeys: smallvec::SmallVec<[Pubkey;4]> }
// SolanaSigner — Solana-Keychain-style abstraction (§9).
pub trait SolanaSigner: Send + Sync { fn pubkey(&self) -> Pubkey; fn try_sign_message(&self, msg: &[u8]) -> Result<Signature, SignerError>; fn backend_kind(&self) -> BackendKind; }
// KillSwitchHandle::signing_enabled — Cheap Acquire-ordered AtomicBool read performed before every sign.
pub fn signing_enabled(&self) -> bool
// KillSwitchHandle::halt — Manual or supervisor-driven halt;
pub fn halt(&self, reason: HaltReason)
// KillSwitchHandle::rearm — Explicit operator re-enable after a trip;
pub fn rearm(&self, operator: &str) -> Result<(), RearmError>
// KillSwitchSupervisor::evaluate — Compares a HealthSnapshot against numeric KillThresholds on each tick;
pub fn evaluate(&mut self, h: &HealthSnapshot) -> Option<HaltReason>
// PreSignCaps::reserve — SYNCHRONOUS, no-I/O.
pub fn reserve(&mut self, lamport_out: u64) -> Result<CapReservation, CapExceeded>
// PreSignCaps::apply_snapshot — Pushed by the sweeper/health loop to reseed lamport_out_budget = min(config_cap, snapshot.spendable_lamports).
pub fn apply_snapshot(&mut self, s: BalanceSnapshot)
// TxShapeValidator::validate — Rejects any tx not matching the arb template: program id not in allowlist, outflow dest != own ATA/hot pubkey, total lamport-out > cap, tip dest not a…
pub fn validate(&self, msg: &VersionedMessage, ctx: &ArbSignContext) -> Result<ValidatedShape, ShapeReject>
// Sweeper::run — Long-running task: on cron tick OR when hot balance > hot_cap_lamports, builds a surplus=balance-working_reserve transfer to the cold treasury, signs…
pub async fn run(self, shutdown: tokio_util::sync::CancellationToken)
// AlertSink::send — Fire-and-forget alert delivery (Telegram/PagerDuty/log) used on halt trips and sweep anomalies.
pub fn send(&self, sev: Severity, msg: &AlertMessage)
```

**Key data structures:**

- **`SignerSidecar`** — `struct SignerSidecar { signer: Box<dyn SolanaSigner>, hot_pubkey: Pubkey, flag: KillSwitchHandle, caps: parking_lot::Mutex<PreSignCaps>, validator: TxShapeValid…` · INVARIANT: the only owner of the hot SolanaSigner.
- **`PreSignCaps`** — `struct PreSignCaps { interval: Duration, max_sigs_per_interval: u32, window_start: Instant, sigs_in_window: u32, lamport_out_budget: u64, lamport_out_used: u64,…` · INVARIANTS: lamport_out_used <= lamport_out_budget always;
- **`CapReservation`** — `struct CapReservation { lamport_out: u64, window_epoch: u64 }` · Returned by reserve();
- **`KillThresholds`** — `struct KillThresholds { revert_rate_pct: f64, revert_min_attempts: u32, revert_window: Duration, realized_loss_sol_per_hr: f64, balance_dev_pct: f64, burn_rate_…` · Day-1 pinned defaults: revert_rate_pct=40.0 (above the §9 >30% infra-bug signal, leaving headroom before auto-halt), revert_min_attempts=20, revert_window=300s, realized_loss_sol_per_hr=0.5, balance_d…
- **`HealthSnapshot`** — `struct HealthSnapshot { revert_rate_pct: f64, attempts_in_window: u32, realized_loss_sol_per_hr: f64, balance_deviation_pct: f64, burn_rate_sol_per_min: f64, ob…` · Fed by the execution/observability module.
- **`BalanceSnapshot`** — `struct BalanceSnapshot { spendable_lamports: u64, expected_lamports: u64, taken_at: Instant }` · spendable = hot balance - working_reserve - rent-exempt minimum.
- **`HaltReason`** — `enum HaltReason { Manual{operator:String}, RevertRate{pct:f64}, RealizedLoss{sol_per_hr:f64}, BalanceDeviation{pct:f64}, BurnRate{sol_per_min:f64} }` · Recorded in TripRecord and used to label metrics and select the runbook branch.
- **`ShapeReject`** — `enum ShapeReject { ProgramNotAllowlisted(Pubkey), ForeignDestination(Pubkey), LamportOutOverCap{requested:u64,cap:u64}, TipNotTipAccount(Pubkey), TipOverCap{req…` · Every variant is a HARD reject -> no sign.
- **`BackendKind`** — `enum BackendKind { Memory, Kms, Squads, Fireblocks }` · Memory is the ONLY kind permitted on the hot sign path (asserted at sidecar construction).
- **`OnCallPosture`** — `enum OnCallPosture { UnmannedAutoHaltOnly, OnCall{ escalate_after: Duration } }` · Makes the §9 on-call expectation explicit.
- **`TripRecord`** — `struct TripRecord { reason: HaltReason, tripped_at: SystemTime, health: HealthSnapshot, acked: bool, acked_by: Option<String> }` · Persisted (append-only file under ops/) so post-trip forensics and the rearm gate (requires acked) survive a restart.

**External crates:** `solana-sdk (Agave-compatible, pin to workspace version) — VersionedMessage, v0::LoadedAddresses, Pubkey, Signature, Keypair`, `solana-program — program-id types shared with onchain allowlist`, `ed25519-dalek (via solana-sdk Keypair) for in-memory hot-key signing`, `zeroize — Zeroizing<Keypair> to wipe hot key on drop`, `parking_lot — fast Mutex for the sign critical section`, `tokio + tokio-util (CancellationToken) — sweeper/supervisor tasks`, `smallvec — route pubkey vectors without heap`, `serde + toml — load ops/config/*.toml`, `prometheus (or metrics+metrics-exporter-prometheus) — SignerMetrics`, `reqwest (rustls) — Telegram/PagerDuty alert sinks`, `tracing — structured logs (never key material)`, `cargo-deny (dev/CI tool, not a lib) — supply-chain gate`, `solana-verify (external CLI) — reproducible build verification`

**Grounded in `plan.md`:** §9 (lines 419-430): key mgmt architecture, hot-key in-memory, blast-radius cap + sweeper, kill-switch + signing-enabled flag, synchronous pre-sign caps, signer tx-shape validation, concrete numeric thresholds + alert routing + on-call + recovery runbook, supply-chain vs opsec threat-model separation; §6 (line 257): deploy posture — upgradeable, Squads upgrade authority, solana-verify reproducible build, upgrade runbook; (line 252) trust-boundary allowlist reused by validator; §10 (lines 462,465-466): burn-rate first-class metric; hot key holds minutes-to-hours of working capital, rest in cold treasury; §11 (Fase 0/1/2 and checklist lines 546-547,560-562,631): signer sidecar, kill-switch, sweeper land in Fase 2; key security baseline + verifiable build in Fase 0

**Tasks (13):**

- **`signer-1` Key security baseline + supply-chain hygiene gate** · Fase 0 · 2d · deps: —
  - Establish hot-key handling and dependency hygiene before any key holds funds. Generate a throwaway chmod-600 hot keypair kept out of git, commit Cargo.lock, add cargo-deny (ops/deny.toml) to CI (RUSTSEC advisories, yanked=deny, source allowlist), pin transitive git deps by rev, and write ops/SUPPLY_CHAIN.md keeping the…
  - **Done when:** cargo-deny check passes in CI and fails the build on an injected yanked/vulnerable dep · hot keypair file is 0600 and absent from git (verified by a CI grep for key material) · SUPPLY_CHAIN.md distinguishes the two threat models and lists the @solana/web3.js trojan lesson + Step Finance opsec lesson without conflating mitigations
- **`signer-2` SolanaSigner trait + MemorySigner hot-key backend** · Fase 0 · 2d · deps: `signer-1`
  - Implement bot/src/signer/keychain.rs: the SolanaSigner trait and a MemorySigner that loads the hot keypair from a 0600 file/env into Zeroizing<Keypair>, signs ed25519 in-process, and zeroizes on drop. Stub KMS/Squads/Fireblocks backends behind BackendKind so the backend is swap-by-config (treasury paths) but assert Mem…
  - **Done when:** MemorySigner produces a valid ed25519 signature verifiable against its pubkey · Keypair memory is zeroized on drop (test via custom Drop-observing wrapper) · Constructing a sidecar with a non-Memory backend on the hot path is rejected at build time/assert
- **`signer-3` TxShapeValidator (allowlist, dest=own-ATA, max-lamport-out, tip)** · Fase 1 · 3d · deps: `signer-2`
  - Implement bot/src/signer/validate.rs. Parse a compiled v0 VersionedMessage, resolve ALT-referenced keys via ArbSignContext.loaded_addresses, and HARD-REJECT: any invoked program id not in the allowlist (CPMM/Whirlpool/PumpSwap + System/SPL/Token-2022/ATA/ComputeBudget from bot/src/config), any lamport/token outflow who…
  - **Done when:** Validator accepts the canonical 2-leg arb (CPMM+Whirlpool) + WSOL wrap/sync/close + tip-inside-tx template · Rejects CPI to a non-allowlisted program, transfer to a foreign destination, lamport-out over cap, tip to a non-tip account, and a signer placed in the ALT · ExpectedOutMismatch fires when the caller under-declares lamport-out · ALT-referenced keys resolve correctly via loaded_addresses
- **`signer-4` Synchronous PreSignCaps (count + cumulative lamport-out)** · Fase 1 · 2d · deps: `signer-2`
  - Implement bot/src/signer/caps.rs: a no-I/O token-bucket for signatures-per-interval plus a cumulative lamport-out budget seeded from BalanceSnapshot (budget = min(config_cap, spendable_lamports)). reserve() rolls the window, checks both ceilings, returns a CapReservation; release() restores budget on dropped tx; apply_…
  - **Done when:** (N+1)th sign in a window is rejected with CapExceeded{count} · A reservation exceeding the lamport-out budget is rejected with CapExceeded{lamport} · release() on a dropped tx restores both count and lamport budget within the same window epoch · reserve() performs zero syscalls (verified by a no-allocation/no-I/O test or bench)
- **`signer-5` SignerSidecar canonical sign path (flag->shape->caps->sign, atomic)** · Fase 1 · 2d · deps: `signer-3`, `signer-4`
  - Implement bot/src/signer/sidecar.rs and mod.rs sign_arb_tx(). Acquire the caps mutex for the whole sequence: read signing-enabled (Acquire), validate shape, reserve caps, then sign via MemorySigner — returning Halted/ShapeRejected/CapExceeded before touching the key if any gate fails. Wire SignerMetrics. Guarantee TOCT…
  - **Done when:** sign_arb_tx returns the correct SignerError variant for each failing gate and never calls the signer when a gate fails · Under concurrent threads, a halt mid-flight cannot let a sign slip through (loom or stress test) · SignerMetrics increments the right labeled counters per outcome · Hot key is never reachable except through sign_arb_tx (no pub accessor)
- **`signer-6` KillSwitch flag + handle (manual halt < seconds, no auto re-arm)** · Fase 2 · 2d · deps: `signer-5`
  - Implement bot/src/signer/killswitch.rs handle: Arc<AtomicBool> signing-enabled read on every sign; halt(reason) flips it sub-second and writes an append-only TripRecord; rearm(operator) requires an acked TripRecord and is never automatic. Expose a CLI/admin trigger so a single manual command halts all outflow in <1s.
  - **Done when:** Manual halt blocks the next sign in <1s (measured) and across all signer-holding tasks · TripRecord is persisted append-only and survives restart; rearm is refused until acked · No code path re-enables signing automatically
- **`signer-7` KillSwitchSupervisor + numeric thresholds + alert routing** · Fase 2 · 3d · deps: `signer-6`
  - Implement killswitch.rs supervisor half + thresholds.rs + alert.rs. evaluate(HealthSnapshot) compares revert-rate (gated by min-attempts), realized-loss/hr, balance-deviation, and burn-rate against pinned numeric KillThresholds (loaded from ops/config/killswitch.toml), auto-halts on first breach, and routes Telegram/Pa…
  - **Done when:** Each threshold (revert-rate>40%/5min over >=20 attempts, loss>0.5 SOL/hr, balance-dev>15%, burn>0.05 SOL/min) auto-trips in a unit test feeding synthetic HealthSnapshots · revert-rate does NOT trip below revert_min_attempts (no low-sample false trip) · Alert sinks fire on trip; a failing sink does not block the halt · Thresholds are config-overridable without code changes
- **`signer-8` Blast-radius sweeper (cron + threshold) to cold treasury** · Fase 2 · 3d · deps: `signer-7`
  - Implement bot/src/signer/sweeper.rs. On a cron tick (every N min) or when hot balance > hot_cap_lamports, build a transfer of surplus=balance-working_reserve to the Squads/KMS treasury, sign it via the sidecar (treasury allowlisted as a sweep destination so shape-validation passes), submit, and apply_snapshot() fresh b…
  - **Done when:** Sweeper moves surplus to treasury on both cron and balance-threshold triggers (Surfpool/mainnet-fork test) · Never drops hot balance below working_reserve + rent-exempt minimum · Sweep tx passes sidecar shape-validation (treasury dest allowlisted) and updates PreSignCaps snapshot · During a kill-switch halt, surplus can still be swept to treasury but no arb signs are allowed
- **`signer-9` Hot-key rotation + working-capital funding ops** · Fase 2 · 2d · deps: `signer-8`
  - Implement ops/scripts/rotate_hot_key.sh: generate new 0600 keypair, fund minimal working capital from treasury via a Squads-approved transfer, hot-swap signer config + restart the sidecar, sweep residual from the old key to treasury, archive/revoke old key. Wire to run on schedule and automatically on a BalanceDeviatio…
  - **Done when:** Rotation script produces a new 0600 key, funds it, swaps config, and sweeps the old key with zero residual · A BalanceDeviation trip triggers rotate-and-sweep as the first containment step · Old key is archived/revoked and never reused
- **`signer-10` Deploy posture: Squads upgrade authority + solana-verify reproducible build** · Fase 2 · 3d · deps: `signer-1`
  - Implement ops/scripts/verify_build.sh (solana-verify build + verify-from-repo so on-chain bytecode matches source) and ops/scripts/deploy_squads.sh (write-buffer, set upgrade authority to the Squads multisig vault, route upgrades through a Squads proposal -> threshold approve -> execute). No unilateral upgrade authorit…
  - **Done when:** solana-verify confirms on-chain program hash == source build hash · Program upgrade authority is the Squads multisig vault (verified via getProgramAccounts/CLI) · An upgrade executes only after meeting the multisig approval threshold; runbook documents rollback
- **`signer-11` Kill-switch recovery runbook + manual-halt drill + on-call posture** · Fase 2 · 2d · deps: `signer-7`, `signer-9`
  - Write ops/runbooks/killswitch_recovery.md: per-HaltReason triage (RevertRate->infra bug, RealizedLoss->sizing/market, BalanceDeviation->suspected compromise rotate+sweep first, BurnRate->fee bleed), re-arm checklist with operator sign-off, and the documented on-call posture (UnmannedAutoHaltOnly vs OnCall escalation).…
  - **Done when:** Runbook has a concrete branch per HaltReason with rotate+sweep-first for suspected compromise · Re-arm requires acked TripRecord + operator sign-off as documented · Manual-halt drill demonstrates outflow stops in <1s and is logged
- **`signer-12` End-to-end signer integration test on mainnet-fork (Surfpool)** · Fase 2 · 3d · deps: `signer-8`, `signer-10`
  - Wire the sidecar into the Jito landing path on Surfpool: real 2-leg arb (CPMM+Whirlpool, then PumpSwap) signed only through sign_arb_tx, caps enforced, kill-switch auto-trip on a synthetic revert spike, sweeper moving surplus to a mock treasury. Assert no outflow ever bypasses the sidecar and that a tampered (foreign-d…
  - **Done when:** Every landed tx in the scenario carries a signature produced by the sidecar (no out-of-band signing path exists) · A synthetic >40%/5min revert spike auto-halts the bot and stops outflow · A maliciously rewritten tx (foreign destination) is rejected before signing · Sweeper evacuates surplus to the mock treasury at threshold
- **`signer-13` FORWARD HOOK: KMS/Fireblocks treasury backend seam (Fase 3+)** · Fase 3 · 1d · deps: `signer-2`
  - Day-1 design seam ONLY: ensure the SolanaSigner trait + BackendKind already make a KMS/Fireblocks treasury-signing backend swap-by-config so Fase 3 sweeper-to-KMS and treasury-managed funding need no hot-path refactor. Implement the KMS backend stub interface and document the boundary; do NOT build full KMS integration…
  - **Done when:** A KMS/Fireblocks backend can be selected by config without changing the hot-path sign code · Boundary is documented; Milestone 1 ships with Memory hot + Squads treasury only · No KMS round-trip is ever introduced on the hot sign path

<details><summary><b>Open questions</b> (7)</summary>

- Exact day-1 numeric values for MAX_LAMPORT_OUT, hot_cap_lamports, and working_reserve_lamports depend on the chosen pre-funded inventory size (§10 'minutes-to-hours of working capital') — proposed as config with placeholder defaults pending the capital decision.
- Squads multisig signer set + approval threshold (e.g. 2-of-3) and who the human signers are is an org/opsec decision not specified in the plan.
- Whether sweeps to treasury should themselves require a second approval or run fully automated from the hot key — current design allows automated sweep-to-treasury-only (treasury is the sole allowed sweep dest), but a compromised hot key could still grief by spamming sweeps; mitigated by rate cap but worth an explicit policy.
- On-call posture default (UnmannedAutoHaltOnly vs OnCall) — plan lists both as options; I defaulted to UnmannedAutoHaltOnly for Milestone 1, to be confirmed.
- Whether to also enforce a per-tx max-CU/max-priority-fee cap inside the validator as an additional fee-bleed guard, or leave fee sizing to the exec module — currently left to exec but the validator could cheaply cap it.
- Persistence backend for TripRecord (append-only file vs sqlite) — file chosen for simplicity; revisit if forensic querying becomes heavy.
- Source of truth for 'own ATAs' set in the validator across rotations — must be regenerated atomically with hot-key rotation; coordination detail between signer-3 and signer-9.

</details>

---

### 5.8 `testing` — Testing harness — LiteSVM, Surfpool mainnet-fork, the rounding-mirror fuzz gate

**Directory:** `tests/`

**Purpose.** The testing harness is the executable safety net and Milestone-1 gate for the atomic-arbitrage system. It proves, before any mainnet lamports are risked, that (a) the on-chain native-Rust `TryArbitrage` program reverts with `Unprofitable` on no-arb inputs and succeeds with an EXACT, predicted balance delta on profitable inputs; (b) the off-chain sizing/quote math reproduces each DEX's on-chain integer math bit-exact across a WIDE fuzzed range, in BOTH swap directions, for BOTH Wave-1 CPMM-class venues (Raydium CPMM, Orca Whirlpool) plus PumpSwap, including the fee-only Token-2022 path — this differential/property test is the HARD GATE for Milestone 1; (c) trust-boundary negative tests hold (…

<details><summary><b>File map</b> (18 files)</summary>

- `tests/Cargo.toml` — Cargo manifest for the test workspace member.
- `tests/common/mod.rs` — Shared test scaffolding re-exported by all integration targets.
- `tests/common/pool_builder.rs` — Synthetic on-chain pool/account constructors for LiteSVM.
- `tests/common/mint_builder.rs` — Builds plain SPL mints and Token-2022 mints with selectable extensions, used by the Token-2022 filter tests and the fee-path differential test.
- `tests/common/swap_harness.rs` — Rust client wrapper that drives the SwapHarness test-only on-chain program.
- `tests/common/cu_budget.rs` — Compute-unit, account-lock, and tx-byte measurement + regression helpers.
- `tests/litesvm_unit.rs` — Core LiteSVM unit tests for TryArbitrage.
- `tests/differential_rounding.rs` — THE MILESTONE-1 GATE.
- `tests/trust_boundary.rs` — Negative trust-boundary tests.
- `tests/token2022_filter.rs` — Token-2022 extension filter tests.
- `tests/surfpool_integration.rs` — Surfpool/surfnet integration tests (feature=surfpool).
- `tests/surfpool_cheatcodes.rs` — Thin typed RPC client for surfnet cheatcodes used by surfpool_integration.rs.
- `tests/historical_replay.rs` — Deterministic historical replay tests (feature=replay).
- `tests/fixtures/snapshots/README.md` — Documents how snapshot fixtures are captured (geyser-grpc-plugin at target slot / Old Faithful CAR export), the on-disk format (account pubkey + base64 data + owner + lam…
- `tests/fixtures/cu_baselines.json` — Committed compute-unit baselines per leg/venue and total-tx, consumed by cu_budget.rs to detect CU regressions (>10% drift fails CI).
- `tests/programs/swap_harness/src/lib.rs` — Test-only native-Rust BPF program that executes exactly ONE swap CPI into a specified Wave-1 venue and records pre/post ATA balances.
- `tests/scripts/run_surfpool.sh` — Helper script (bash) to launch surfnet as a lazy mainnet-fork against a configured RPC, wait until healthy, and export the endpoint for surfpool_integration.rs.
- `tests/README.md` — Operator doc for the harness: how to run each tier (fast LiteSVM `cargo test`, gated `--features surfpool`, `--features replay`), what gates Milestone 1 (the differential…

</details>

**Public interfaces (10):**

```rust
// new_svm — Bootstraps a LiteSVM instance with arb-program, SwapHarness, SPL Token, and Token-2022 BPF blobs loaded;
pub fn new_svm() -> TestEnv
// prewarm_alt — Creates an Address Lookup Table, extends it, then warps the slot forward by >=1 so the table is usable;
pub fn prewarm_alt(env: &mut TestEnv, addrs: &[Pubkey]) -> Pubkey
// run_single_swap — Drives the SwapHarness program to perform one swap CPI into `venue`/`dir`;
pub fn run_single_swap(env: &mut TestEnv, venue: Venue, dir: Direction, amount_in: u64, pool: &PoolAccounts) -> Result<RealizedSwap, FailedTransactionMetadata>
// quote_out — Re-exported from crates/sizing (the off-chain venue under test).
pub fn quote_out(venue: Venue, dir: Direction, reserve_in: u128, reserve_out: u128, fee_bps: u32, amount_in: u64) -> u64
// measure_legs — Extracts compute_units_consumed and per-leg attribution from a successful TryArbitrage TransactionMetadata for CU budgeting and baseline regression.
pub fn measure_legs(meta: &TransactionMetadata) -> LegCu
// assert_tx_within_limits — Asserts account_locks < 128, serialized size <= 1232 bytes, and (via accompanying meta) CU < 1_400_000.
pub fn assert_tx_within_limits(tx: &VersionedTransaction)
// build_cpmm / build_whirlpool / build_pumpswap — Installs a synthetic pool with chosen reserves/fees into LiteSVM and returns the strict remaining_accounts ordering the on-chain program consumes.
pub fn build_cpmm(env: &mut TestEnv, spec: CpmmPoolSpec) -> PoolAccounts
// surfnet cheatcode client — Typed wrappers over surfnet_* JSON-RPC cheatcodes for the mainnet-fork integration tier.
pub async fn set_account(c:&RpcClient,k:Pubkey,a:AccountData); pub async fn clone_program_account(c:&RpcClient,p:Pubkey); pub async fn time_travel(c:&RpcClient,slot:u64); pub async fn profile_transaction(c:&RpcClient,tx:&VersionedTransaction)->TxProfile
// load_snapshot — Loads a Yellowstone/Old Faithful pre-state snapshot from disk into installable (Pubkey, Account) pairs for deterministic replay.
pub fn load_snapshot(path: &Path) -> Vec<(Pubkey, Account)>
// t22_transfer_fee_mint / t22_with_extension — Construct Token-2022 mints: the first an ALLOWED fee-only mint for the differential fee-path case;
pub fn t22_transfer_fee_mint(env:&mut TestEnv, bps:u16, max_fee:u64) -> Pubkey; pub fn t22_with_extension(env:&mut TestEnv, ext: BadExt) -> Pubkey
```

**Key data structures:**

- **`TestEnv`** — `struct TestEnv { svm: LiteSVM, bot_authority: Keypair, payer: Keypair, wsol_ata: Pubkey, usdc_ata: Pubkey, arb_program_id: Pubkey, harness_program_id: Pubkey, c…` · Invariant: bot_authority owns wsol_ata and usdc_ata (trust-boundary tests rely on this);
- **`Venue`** — `enum Venue { RaydiumCpmm, OrcaWhirlpool, PumpSwap }` · PumpSwap is present in the type from day 1 but only RaydiumCpmm+OrcaWhirlpool are required-green for the Milestone-1 differential gate;
- **`Direction`** — `enum Direction { AtoB, BtoA }` · Differential test MUST cover both variants per venue;
- **`PoolAccounts`** — `struct PoolAccounts { state: Pubkey, vault_in: Pubkey, vault_out: Pubkey, extra: Vec<Pubkey>, remaining_accounts: Vec<AccountMeta>, reserve_in: u128, reserve_ou…` · remaining_accounts follows the program's strict ordering convention;
- **`RealizedSwap`** — `struct RealizedSwap { realized_out: u64, realized_in: u64, cu_consumed: u64 }` · realized_out is the ACTUAL post-minus-pre ATA balance delta (Token-2022 net of fee), NOT the instruction amount — this is the on-chain side of the bit-exact comparison and honors the balance-delta pro…
- **`CpmmPoolSpec / WhirlpoolSpec / PumpSwapSpec`** — `struct CpmmPoolSpec { reserve_a: u128, reserve_b: u128, fee_bps: u32, mint_a: MintKind, mint_b: MintKind }  (Whirlpool adds sqrt_price:u128, tick_spacing:u16, t…` · MintKind ::PlainSpl | ::FeeOnlyT22{bps,max} | ::Bad(BadExt) lets pool builders parametrize the Token-2022 path;
- **`BadExt`** — `enum BadExt { TransferHook, NonTransferable, DefaultAccountStateFrozen, MemoTransfer, ConfidentialTransfer, PermanentDelegate, MintCloseAuthority }` · Exactly the HARD-REJECT set from the invariants;
- **`LegCu`** — `struct LegCu { total: u64, leg_a: u64, leg_b: u64, assert_overhead: u64 }` · Used to assert total < 1_400_000 and to feed cu_baselines.json regression detection.
- **`SnapshotRecord`** — `struct SnapshotRecord { pubkey: Pubkey, owner: Pubkey, lamports: u64, data_b64: String, slot: u64, write_version: u64 }` · On-disk replay fixture format;

**External crates:** `litesvm (LiteSVM in-process SVM test substrate — primary unit/property tier, per §8)`, `solana-sdk / solana-program (tx construction, VersionedTransaction, sysvars, AccountMeta)`, `spl-token, spl-token-2022, spl-associated-token-account (mint/ATA construction incl. extensions)`, `proptest (1.x) (property/fuzz driver for the differential rounding test — wide-range (reserves,fees,amount_in) with shrinking)`, `arbitrary (structured fuzz input generation for pool/mint specs)`, `criterion (optional, CU/throughput benchmark target only)`, `surfnet/surfpool client + jsonrpsee or solana RpcClient (mainnet-fork integration tier cheatcodes — feature-gated)`, `Old Faithful / Jetstreamer CAR reader or geyser snapshot loader (replay tier — feature-gated)`

**Grounded in `plan.md`:** §8 (testing substrate: LiteSVM, Surfpool cheatcodes, solana-test-validator --clone, deterministic replay via Yellowstone/Old Faithful, devnet-not-allowed); §11 Fase 0 (toolchain install + skeleton build + mainnet-fork clone checklist); §11 Fase 1 (differential/property rounding-mirror gate + full Fase-1 checklist); §11 Fase 2 (PumpSwap addition — forward hook); §12 (Definition of Done: revert on LiteSVM+Surfpool+mainnet-small, per-venue two-direction fuzz proof, trust boundary, CU/lock/byte limits, verifiable build); §1-3 (system context: atomicity = runtime revert, on-chain assert as gate, no mempool)

**Tasks (10):**

- **`testing-1` Fase 0: toolchain + LiteSVM bootstrap + skeleton build** · Fase 0 · 2d · deps: `arb-program skeleton crate must exist (program module Fase-0 task); reference buffalojoec/arb-program`
  - Install/verify Rust, solana-cli, Anchor (tooling only), LiteSVM, and Surfpool. Stand up tests/Cargo.toml as a workspace member, implement tests/common/mod.rs new_svm() loading SPL Token + Token-2022 BPF blobs, and get buffalojoec/arb-program skeleton building and its own tests passing locally as a reference. Verify all…
  - **Done when:** `cargo test -p tests` compiles and runs a trivial new_svm() test · SPL Token and Token-2022 programs load into LiteSVM without error · All three Wave-1 program ids (CPMMoo8L..., whirLbM..., pAMMBay...) present in shared config and verified on-chain
- **`testing-2` Pool + mint builders for LiteSVM** · Fase 1 · 4d · deps: `testing-1`, `crates/sizing reserve/fee field conventions (sizing Fase-1 task)`
  - Implement tests/common/pool_builder.rs (build_cpmm/build_whirlpool with selectable reserves/fees and correct remaining_accounts ordering) and tests/common/mint_builder.rs (plain SPL, fee-only Token-2022, and one constructor per BadExt). These are the substrate for all functional + property tests.
  - **Done when:** build_cpmm and build_whirlpool install pools that the real venue math (off-chain quote) can read identically · remaining_accounts ordering matches the on-chain program's strict convention (verified by a smoke swap) · Each BadExt and the fee-only T22 mint construct successfully
- **`testing-3` SwapHarness test program + single-leg client** · Fase 1 · 3d · deps: `testing-2`
  - Write tests/programs/swap_harness (native-Rust BPF) that performs ONE swap CPI into a chosen venue/direction and records pre/post ATA balances to a return account. Implement tests/common/swap_harness.rs run_single_swap returning RealizedSwap{realized_out, cu}. This isolates each leg for the differential gate.
  - **Done when:** A single CPMM swap via SwapHarness returns realized_out equal to the pool's actual vault delta · Token-2022 fee-only swap returns realized_out NET of transfer fee (balance delta, not instruction amount) · cu_consumed populated from TransactionMetadata
- **`testing-4` LiteSVM unit tests: revert + exact-delta + boundary** · Fase 1 · 3d · deps: `testing-2`, `testing-3`, `crates/sizing quote_out (sizing Fase-1)`, `arb-program TryArbitrage instruction`
  - Implement tests/litesvm_unit.rs: no-arb => FailedTransactionMetadata with Unprofitable and zero net token movement; profitable => success with realized delta == off-chain predicted delta exactly; min_profit/costs boundary (revert at predicted-1, succeed at predicted); exercise warp_to_slot and set_sysvar<Clock>.
  - **Done when:** Unprofitable input asserts FailedTransactionMetadata + Unprofitable error variant + no net token movement · Profitable input asserts success AND exact predicted balance delta · Boundary test: post == pre+min_profit-1 reverts; == +0 succeeds
- **`testing-5` MILESTONE-1 GATE: differential/property rounding-mirror test** · Fase 1 · 5d · deps: `testing-3`, `testing-2`, `crates/sizing per-venue quote + rounding mirror (sizing Fase-1)`
  - Implement tests/differential_rounding.rs. proptest fuzzes (reserve_a, reserve_b, fee_bps, amount_in) over a wide range, for BOTH directions and BOTH required venues (Raydium CPMM, Orca Whirlpool — per-venue math, NOT shared), asserting off-chain quote_out == on-chain run_single_swap realized_out bit-exact (Floor output…
  - **Done when:** For Raydium CPMM and Orca Whirlpool, both directions, thousands of fuzzed cases: predicted_out == realized_out with zero mismatches · Token-2022 fee-only case: predicted net-out == realized balance delta · Shrinking produces a minimal counterexample on any divergence (proves the harness catches rounding bugs) · PumpSwap cases compile behind feature flag, excluded from Milestone-1 gate
- **`testing-6` Trust-boundary + Token-2022 filter negative tests** · Fase 1 · 3d · deps: `testing-2`, `testing-4`, `arb-program allowlist + extension filter`
  - Implement tests/trust_boundary.rs (reject swap-CPI to non-allowlisted program id supplied via remaining_accounts; reject balance-read from a token account not owned by bot authority; revert holds regardless of preflight) and tests/token2022_filter.rs (HARD-REJECT each BadExt incl MintCloseAuthority; ACCEPT plain SPL +…
  - **Done when:** Griefer program id in remaining_accounts => deterministic revert with specific error · Foreign-owned balance account => revert · Each of the 7 BadExt mints rejected with the correct error; fee-only T22 + plain SPL accepted · Assertions identical under skip-preflight semantics (on-chain assert is the only net)
- **`testing-7` CU / account-lock / tx-byte budget + ALT pre-warm asserts** · Fase 1 · 2d · deps: `testing-4`, `tx-builder module producing v0 tx + ALT (txbuilder Fase-1)`
  - Implement tests/common/cu_budget.rs and integrate into the suites: assert total CU < 1_400_000, account locks < 128, serialized v0 tx <= 1232 bytes, and that the ALT is pre-warmed >=1 slot before use (prewarm_alt panics on extend-then-use-same-slot). Establish tests/fixtures/cu_baselines.json and fail CI on >10% per-le…
  - **Done when:** Limits test asserts locks<128, bytes<=1232, CU<1.4M with precise failure messages · prewarm_alt enforces no extend-then-use same-slot (asserted by a negative case) · CU baseline regression catches a deliberately-injected extra-account change
- **`testing-8` Surfpool mainnet-fork integration vs real Raydium/Orca** · Fase 1 · 5d · deps: `testing-4`, `testing-6`
  - Implement tests/surfpool_cheatcodes.rs (typed surfnet_* client), tests/scripts/run_surfpool.sh, and tests/surfpool_integration.rs: lazy-fork mainnet, clone REAL Raydium CPMM + Orca Whirlpool pools via cheatcodes, fund bot WSOL/USDC inventory, run TryArbitrage, assert revert on intentionally-unprofitable cloned state an…
  - **Done when:** Real Raydium CPMM and Orca Whirlpool pools clone into surfnet and TryArbitrage executes against them · Unprofitable cloned state reverts with Unprofitable; engineered-profitable state lands with no stuck inventory · profileTransaction CU within agreed tolerance of LiteSVM measurement · Validates the 'revert on intentionally-unprofitable input in mainnet-fork' checklist item
- **`testing-9` Deterministic historical replay (Yellowstone / Old Faithful)** · Fase 1 · 3d · deps: `testing-5`, `testing-8`
  - Implement tests/historical_replay.rs + fixtures/snapshots/ loader: capture a mainnet pre-state snapshot at a target slot (geyser-grpc-plugin or Old Faithful CAR), install the exact account set, replay TryArbitrage, and assert the predicted revert/profit decision is bit-identical to the recorded outcome. Document the ca…
  - **Done when:** At least one captured snapshot replays bit-identically (same revert/profit decision) · Snapshot format documented and regenerable · Replay also exercises slot+write_version dedupe expectations from the detection contract
- **`testing-10` Fase-2 forward hook: PumpSwap differential + Surfpool clone** · Fase 2 · 3d · deps: `testing-5`, `testing-8`
  - FORWARD HOOK (do NOT gate Milestone 1). Enable the PumpSwap (pAMMBay...) cases already stubbed in the venue-parametric differential test and pool builders, add Surfpool cloning of real PumpSwap pools, and extend cu_baselines. Confirms the day-1 venue-parametric seam (Venue enum, per-venue builders) absorbs PumpSwap wit…
  - **Done when:** PumpSwap both-direction differential passes bit-exact · Real PumpSwap pool clones and runs in surfnet · No redesign of harness interfaces required to add the venue (validates the seam)

<details><summary><b>Open questions</b> (7)</summary>

- Exact realized_out extraction contract from the on-chain DEX CPIs: confirmed approach is reading pre/post ATA balance deltas via SwapHarness, but if crates/sizing exposes a venue trait the harness should consume that trait directly rather than re-deriving inputs — needs alignment with the sizing module owner.
- Which specific mainnet slot(s) to capture as canonical replay fixtures, and whether snapshots are stored in-repo (small) or fetched from object storage in CI (large CAR files).
- Whether Surfpool/surfnet supports cloning ALL three Wave-1 programs' full account graphs (tick arrays, oracle accounts) reliably enough for deterministic asserts, or whether some legs must remain LiteSVM-only with synthetic pools.
- CU baseline drift tolerance (proposed 10%) and whether to fail-hard or warn in CI until the program stabilizes.
- Token-2022 fee-only differential: confirm whether on-chain swap programs withhold transfer fee on the OUT leg, the IN leg, or both, since the off-chain net-of-fee math must mirror the exact withholding point — venue/program-specific.
- Whether the differential test must also cover three-decimal/extreme-decimal mints, or if Milestone-1 scope is fixed-decimal WSOL/USDC only.
- Surfpool determinism for profileTransaction CU vs LiteSVM: acceptable tolerance band before flagging a discrepancy as a real bug.

</details>

---

### 5.9 `observ` — Observability & economics — metrics, probabilistic cost-gate, golden-replay regression

**Directory:** `bot/src/metrics/ + analytics/`

**Purpose.** Provide the first-class health/PnL telemetry, the probabilistic unit-economics cost-gate that runs BEFORE every sign, and the golden-replay/backtest regression gate that proves predicted-vs-realized accuracy before capital is committed. This module turns the spec's hard invariants (revert-rate >30% = infra bug; burn-rate lamports/min on reverted losers; submit-latency P50/P95; confirmation rank; realized slippage per route; PnL) into a concrete in-process metrics pipeline plus an off-process analytics/dashboard surface, and exposes the E[net per opportunity] model as a synchronous pre-sign gate consumed by the signer/kill-switch.

<details><summary><b>File map</b> (19 files)</summary>

- `bot/src/metrics/mod.rs` — Crate-internal module root;
- `bot/src/metrics/registry.rs` — Lock-free in-process metric registry: counters/gauges/histograms backed by AtomicU64 / quantile sketches.
- `bot/src/metrics/counters.rs` — Defines the canonical metric keys/labels and thin typed wrappers so callers never pass raw strings.
- `bot/src/metrics/latency.rs` — Submit-latency P50/P95 tracking across pipeline stages + confirmation-rank capture.
- `bot/src/metrics/slippage.rs` — Realized slippage per route: compare predicted_out (from sizing engine) vs realized_out (balance delta) per venue-pair+direction, store as bps histogram.
- `bot/src/metrics/pnl.rs` — PnL ledger: realized lamports per tx, separated landed-profit vs burned-loss, per token;
- `bot/src/metrics/health.rs` — Computes HealthSnapshot from rolling windows and emits KillSwitchSignal when numeric thresholds trip.
- `bot/src/metrics/econ.rs` — THE probabilistic unit-economics model + synchronous cost-gate.
- `bot/src/metrics/exporter.rs` — Off-hot-path HTTP exporter serving Prometheus text exposition + a /healthz JSON.
- `bot/src/metrics/alerts.rs` — Deviation-alert router: takes KillSwitchSignal/HealthSnapshot deltas and dispatches to Telegram/PagerDuty webhooks per §9 alert-routing requirement.
- `bot/src/metrics/config.rs` — Typed config loaded from infra config: thresholds, EconParams, p_land prior, exporter bind addr, alert sinks.
- `analytics/Cargo.toml` — Separate binary crate for off-line backtest/golden-replay regression gate (NOT in the hot-path bot binary).
- `analytics/src/main.rs` — CLI entrypoint: `analytics backtest`, `analytics gate`, `analytics replay`.
- `analytics/src/corpus.rs` — Golden-replay corpus loader/format: 10-50 historical arb opportunities (winners+losers) captured from Geyser snapshot / Old Faithful, with frozen pool state + recorded re…
- `analytics/src/replay.rs` — Replays each GoldenSample through the SAME off-chain sizing+CostModel used live, comparing predicted vs recorded realized;
- `analytics/src/backtest.rs` — Aggregate backtest: runs the probabilistic model over the corpus to estimate realized E[net], revert-rate, burn-rate vs predicted;
- `analytics/src/report.rs` — Renders ReplayResult/BacktestReport to a static HTML/markdown dashboard artifact and a CI-friendly summary line.
- `analytics/dashboards/grafana-arbit-health.json` — Grafana dashboard definition for the live Prometheus scrape: revert-rate, burn-rate, submit P50/P95, confirmation rank, realized slippage per route, PnL panels with devia…
- `analytics/corpus/README.md` — Documents how to capture golden samples (Geyser snapshot / Old Faithful), the JSON schema, and the winner/loser balance requirement.

</details>

**Public interfaces (16):**

```rust
// MetricsRegistry::record_attempt — Hot-path: increment arb_attempts_total.
pub fn record_attempt(&self)
// MetricsRegistry::record_land — Hot-path: a bundle landed profitably.
pub fn record_land(&self, profit_lamports: u64, route: RouteKey, slot: u64, index_in_block: u32)
// MetricsRegistry::record_revert — Hot-path: an attempt reverted/dropped.
pub fn record_revert(&self, cause: RevertCause, burned_lamports: u64)
// metrics::start_span — RAII latency timer;
pub fn start_span(stage: LatencyStage) -> SpanGuard
// metrics::record_realized_slippage — Post-land: records bps deviation of realized vs bit-exact predicted output per venue-pair+direction.
pub fn record_realized_slippage(route: RouteKey, predicted_out: u64, realized_out: u64)
// CostModel::e_net — Pure probabilistic expected-value: p_land*(spread - swap_fees - flash_fee - tip - prio - base) - (1-p_land)*(prio+base) - rent_churn - E[rug/honeypot]…
pub fn e_net(&self, inputs: &CostInputs) -> i128
// CostModel::gate — Synchronous cost-gate.
pub fn gate(&self, inputs: &CostInputs) -> CostGateDecision
// PLandEstimator::estimate — EWMA landing probability for a (route, tip-bucket), seeded with conservative prior p0 until enough samples;
pub fn estimate(&self, route: RouteKey, tip_bucket: TipBucket) -> f64
// PLandEstimator::update — Feeds landing outcomes back into the EWMA so the cost-gate adapts to live landing rate.
pub fn update(&self, route: RouteKey, tip_bucket: TipBucket, landed: bool)
// HealthEvaluator::evaluate — Computes HealthSnapshot and returns Trip{reason} if any numeric threshold (revert-rate>30%/5m, burn-rate, realized-loss/hr, balance deviation) is exce…
pub fn evaluate(&self, pnl: &PnlLedger, reg: &MetricsRegistry, hot_key_balance: u64) -> KillSwitchSignal
// HealthEvaluator::snapshot — Read-only health snapshot for /healthz and dashboards.
pub fn snapshot(&self, pnl: &PnlLedger, reg: &MetricsRegistry) -> HealthSnapshot
// PnlLedger::record_outcome — Append a per-tx economic outcome (landed profit or reverted burn) for PnL and burn-rate aggregation.
pub fn record_outcome(&self, outcome: TxOutcome)
// exporter::serve — Spawns the Prometheus /metrics + /healthz HTTP server off the hot path.
pub async fn serve(addr: SocketAddr, registry: &'static MetricsRegistry, health: HealthEvaluator) -> anyhow::Result<()>
// AlertRouter::dispatch — Routes a trip/deviation to Telegram/PagerDuty with dedup + runbook link.
pub async fn dispatch(&self, signal: KillSwitchSignal, snap: &HealthSnapshot)
// analytics::replay::replay — Golden-replay regression: predicted (via live sizing+CostModel) vs recorded realized;
pub fn replay(samples: &[GoldenSample], tolerance_bps: i64, model: &CostModel) -> Vec<ReplayResult>
// analytics::backtest::run_backtest — Aggregate predicted vs realized E[net], revert-rate, burn-rate over corpus to confirm unit-economics before infra spend.
pub fn run_backtest(samples: &[GoldenSample], model: &CostModel) -> BacktestReport
```

**Key data structures:**

- **`CostInputs`** — `struct CostInputs { spread_lamports:u64, swap_fees_lamports:u64, flash_fee_lamports:u64, tip_lamports:u64, prio_lamports:u64, base_lamports:u64, p_land:f64 }` · All economic terms in lamports;
- **`CostGateDecision`** — `enum CostGateDecision { Proceed{ e_net_lamports:i128 }, Reject{ reason:RejectReason, e_net_lamports:i128 } }` · i128 because intermediate p_land-weighted terms can be large/negative.
- **`RejectReason`** — `enum RejectReason { NegativeExpectedValue, BelowMinEdge, TipExceedsProfitFraction }` · TipExceedsProfitFraction enforces §9 'cap tip sebagai fraksi profit'.
- **`EconParams`** — `struct EconParams { rent_churn_lamports:u64, e_rug_honeypot_lamports:u64, tip_profit_fraction_cap:f64, min_edge_lamports:i128 }` · rent_churn covers ALT/ATA open-close churn (§10 rent_churn(ALT));
- **`PLandEstimator`** — `struct PLandEstimator { prior:f64, alpha:f64, buckets:DashMap<(RouteKey,TipBucket), EwmaState> } where EwmaState{ p:f64, n:u64 }` · EWMA with smoothing alpha;
- **`TxOutcome`** — `struct TxOutcome { sig:Signature, kind:TxKind, token:Pubkey, gross_lamports:i64, fees_paid:u64, tip_paid:u64, prio:u64, base:u64, burned_lamports:u64, slot:u64,…` · Invariant: kind==Reverted => tip_paid==0 (tip inside atomic tx, unpaid on revert, §9) AND burned_lamports==prio+base.
- **`RevertCause`** — `enum RevertCause { TipLost, Congestion, StaleBlockhash, SimFail, OnchainUnprofitable, Unknown }` · Mirrors §11 Fase2 'track penyebab drop (tip kalah/kongesti/stale/sim-fail)'.
- **`Thresholds`** — `struct Thresholds { revert_rate_pct:f64, revert_window:Duration, burn_rate_lamports_per_min_max:u64, realized_loss_sol_per_hour_max:f64, hot_key_balance_dev_lam…` · revert_rate_pct default 30.0 over 5min (§9).
- **`KillSwitchSignal`** — `enum KillSwitchSignal { Healthy, Trip{ reason:TripReason } }` · Consumed by signer/supervisor to flip signing-enabled.
- **`HealthSnapshot`** — `struct HealthSnapshot { revert_rate_pct:f64, burn_rate_lpm:u64, submit_p50:Duration, submit_p95:Duration, realized_pnl_lamports:i64, attempts:u64, lands:u64, la…` · The canonical dashboard/healthz payload;
- **`RouteKey`** — `struct RouteKey { venue_pair:[Pubkey;2], direction:Direction } where Direction{ AtoB, BtoA }` · Identifies a venue pair (e.g.
- **`GoldenSample`** — `struct GoldenSample { id:String, slot:u64, pool_states:Vec<PoolSnapshot>, route:RouteKey, amount_in:u64, recorded_realized_out:u64, recorded_landed:bool, record…` · Frozen historical opportunity (winner or loser).
- **`ReplayResult`** — `struct ReplayResult { id:String, predicted_out:u64, realized_out:u64, abs_bps_dev:i64, predicted_e_net:i128, recorded_net:i64, within_tolerance:bool }` · within_tolerance=false on ANY sample fails the CI gate;

**External crates:** `once_cell (Lazy global registry)`, `hdrhistogram (P50/P95 streaming quantiles)`, `dashmap (per-route p_land EWMA map, lock-free reads)`, `hyper (lightweight exporter HTTP server)`, `tokio (exporter + alert tasks; workspace-pinned)`, `serde / serde_json (config, corpus, reports)`, `solana-sdk (Pubkey/Signature; pinned to workspace Agave version)`, `clap (analytics CLI)`, `statrs (tolerance/regression statistics in backtest)`, `reqwest (Telegram/PagerDuty webhook dispatch in alerts; rustls)`, `anyhow / thiserror (errors)`

**Grounded in `plan.md`:** §9 lines 411-435; §10 lines 454-477; §11 Fase 1 lines 505-533; §11 Fase 2 lines 536-563 (esp. line 548); §11 Fase 4 lines 589-608 (esp. line 598/607); §12 lines 622-624

**Tasks (14):**

- **`observ-1` Metric registry + canonical keys (lock-free, allocation-free hot path)** · Fase 1 · 2d · deps: —
  - Build registry.rs + counters.rs: global once_cell registry of AtomicU64 counters/gauges and hdrhistogram quantile sketches. Define the canonical metric names/labels and typed record_* wrappers so callers never pass raw strings. Hot-path writers must not allocate or lock.
  - **Done when:** record_attempt/record_revert/record_land are lock-free and allocation-free (verified by test allocator) · All canonical metric names from §11 line 548 (revert, latency, slippage, PnL, confirmation rank) exist as typed wrappers · Concurrent writers from 8 threads produce correct counts under loom or a stress test
- **`observ-2` Latency spans (P50/P95) + confirmation-rank capture** · Fase 1 · 1.5d · deps: `observ-1`
  - Implement latency.rs: RAII SpanGuard per LatencyStage (DetectToBuild/BuildToSim/SimToSubmit/SubmitToLand) feeding submit_latency_seconds histograms; ConfirmationRank{slot,index_in_block} capture from landed bundles.
  - **Done when:** P50/P95 computed within 1% of reference quantile on a synthetic distribution · SpanGuard observes on Drop even on early return/panic-unwind · Confirmation rank captured as (slot,index) and exposed in snapshot
- **`observ-3` PnL ledger + burn-rate accumulator** · Fase 1 · 2d · deps: `observ-1`
  - Implement pnl.rs: append-only ring of TxOutcome with atomic aggregates for realized PnL and a windowed burn-rate (lamports/min on reverted losers). Enforce invariants: reverted => tip_paid==0 and burned==prio+base.
  - **Done when:** burn_lamports_window(60s) matches manual sum on a seeded outcome stream · Invariant test rejects a Reverted outcome with tip_paid>0 · Realized PnL = sum(landed gross) - sum(burned) over the window
- **`observ-4` Probabilistic cost model + synchronous cost-gate** · Fase 1 · 2.5d · deps: —
  - Implement econ.rs CostModel::e_net and ::gate exactly per §10: E[net]=p_land*(spread-fees-flash-tip-prio-base) - (1-p_land)*(prio+base) - rent_churn - E[rug]. Pure, allocation-free, deterministic (Q32.32 p_land) so it is callable on the signer hot path. Enforce tip<=fraction*profit and min_edge.
  - **Done when:** e_net reproduces the §10 formula term-for-term (table-driven tests) · gate() Rejects when expected value < min_edge and when tip exceeds the profit-fraction cap · A worked liquid-pair example with avg $1.58 + 60% tip leakage gates to Reject (NegativeExpectedValue) · gate() is allocation-free (test allocator) so signer can call it synchronously
- **`observ-5` p_land EWMA estimator (per route + tip bucket)** · Fase 2 · 2d · deps: `observ-4`, `observ-3`
  - Implement PLandEstimator: EWMA landing probability bucketed by (RouteKey,TipBucket), seeded with a conservative prior until min_samples, updated by land/revert outcomes. Feeds CostInputs.p_land.
  - **Done when:** estimate() returns prior until min_samples reached, then EWMA · update() converges to true land-rate on a Bernoulli stream within tolerance · Distinct tip buckets maintain independent estimates
- **`observ-6` Health evaluator + numeric kill-switch thresholds** · Fase 2 · 2d · deps: `observ-2`, `observ-3`
  - Implement health.rs: HealthEvaluator computes HealthSnapshot and emits KillSwitchSignal::Trip on revert-rate>30%/5m, burn-rate, realized-loss/hr, or hot-key balance deviation. All numeric, configurable. This is the metric-side of the kill-switch; the signer module owns the signing-enabled flag.
  - **Done when:** revert-rate>30% over 5m window emits Trip{RevertRateExceeded} · burn-rate and realized-loss thresholds each trip at their numeric boundary in tests · evaluate() is read-only over ledger/registry (no mutation) · Default thresholds loaded from config match spec (30% revert)
- **`observ-7` Prometheus exporter + /healthz (off hot path)** · Fase 2 · 1.5d · deps: `observ-1`, `observ-6`
  - Implement exporter.rs: tokio task serving /metrics (Prometheus text) and /healthz (HealthSnapshot JSON) on localhost. Never touched by the signing hot path. Keeps infra cost at the §10 ladder (self-hosted scrape, no SaaS).
  - **Done when:** GET /metrics returns valid Prometheus exposition parseable by a prometheus text parser · GET /healthz returns HealthSnapshot JSON · Exporter runs on a dedicated task and a load test shows zero added latency to record_* calls
- **`observ-8` Deviation-alert router (Telegram/PagerDuty) + runbook links** · Fase 2 · 1.5d · deps: `observ-6`
  - Implement alerts.rs: AlertRouter dispatches KillSwitchSignal/deviation to Telegram/PagerDuty with per-reason dedup and a runbook URL, off hot path. Satisfies §9 alert-routing + on-call + runbook requirement.
  - **Done when:** A Trip dispatches exactly one alert per dedup window per reason · Each TripReason carries a runbook URL in the payload · Webhook failure is logged and retried without blocking the evaluator
- **`observ-9` Realized-slippage-per-route instrumentation** · Fase 2 · 1d · deps: `observ-1`
  - Implement slippage.rs: record_realized_slippage(route,predicted_out,realized_out) on every landed tx, storing signed bps into realized_slippage_bps{venue_pair,direction}. predicted_out comes from the bit-exact sizing mirror so nonzero slippage is a real signal.
  - **Done when:** bps computed as ((predicted-realized)*10000)/predicted with saturating signed arithmetic · Distinct (venue_pair,direction) routes bucket independently · On a landed tx where predicted==realized (post bit-exact mirror), recorded slippage==0
- **`observ-10` Golden-replay corpus format + loader** · Fase 1 · 1.5d · deps: —
  - Create analytics crate skeleton + corpus.rs: GoldenSample JSON schema and loader for 10-50 frozen historical opportunities (winners+losers) with pool state, route, amount_in, and recorded realized outcome. Document capture via Geyser snapshot/Old Faithful. Day-1 seam so the Fase-1 fuzz oracle feeds it.
  - **Done when:** load_corpus parses sample winner+loser fixtures · Schema documents both profitable and reverting samples (loser-burn coverage) · analytics binary builds and depends on bot crate (reuses CostModel)
- **`observ-11` Golden-replay regression gate (predicted vs realized) — CI-blocking** · Fase 2 · 2.5d · deps: `observ-10`, `observ-4`
  - Implement replay.rs + report.rs + `analytics gate` subcommand: replay each GoldenSample through the SAME live sizing+CostModel, compare predicted_out vs recorded_realized_out within tolerance_bps; exit nonzero on any failure so capital commit is blocked. Reuses the bit-exact Fase-1 mirror oracle.
  - **Done when:** gate exits nonzero if any sample's predicted_out deviates beyond tolerance from recorded realized · Reuses bot::sizing mirror + bot::metrics::econ::CostModel (no forked math) · Report shows per-sample predicted vs realized + pass/fail banner · CI integration documented so the gate runs before any capital-committing deploy
- **`observ-12` Aggregate backtest + unit-economics confirmation report** · Fase 2 · 1.5d · deps: `observ-10`, `observ-4`, `observ-5`
  - Implement backtest.rs + `analytics backtest`: run CostModel over the corpus to estimate realized E[net], revert-rate, burn-rate vs predicted, surfacing model bias. Produces the 'confirm unit-economics before infra spend' artifact (§10 line 476).
  - **Done when:** BacktestReport reports predicted_e_net_total, realized_net_total, realized revert-rate + burn-rate, and model_bias · Run over a corpus dominated by losers yields a negative/near-zero realized net consistent with §10 (liquid pairs negative after loser-burn) · Output is human-readable + machine-parseable JSON
- **`observ-13` Grafana dashboard + deviation alert rules** · Fase 2 · 1d · deps: `observ-7`, `observ-9`
  - Author analytics/dashboards/grafana-arbit-health.json: panels for revert-rate, burn-rate, submit P50/P95, confirmation rank, realized slippage per route, PnL; alert rules (revert>30%/5m, burn-rate>max) wired to the same routing as alerts.rs. Self-hosted Grafana to stay on the §10 cost ladder.
  - **Done when:** Dashboard imports cleanly against the exporter's metric names · Revert-rate panel has a visual 30% threshold line + alert rule · Burn-rate and PnL panels present and populated from /metrics in a local Prometheus+Grafana stack
- **`observ-14` Wire cost-gate into signer pre-sign + health into kill-switch (integration seam)** · Fase 2 · 1.5d · deps: `observ-4`, `observ-6`
  - Integration task: expose CostModel::gate for the signer's synchronous pre-sign check and route KillSwitchSignal::Trip to the supervisor that flips signing-enabled. This module provides the gate + signal; the signer module (key-mgmt) owns the flag and synchronous lamport-out cap. Mark as the day-1 seam between metrics a…
  - **Done when:** A fake signer rejects an EV-negative CostInputs via gate() before signing · A simulated revert-rate spike produces Trip consumed by the fake supervisor to set signing-enabled=false · Contract doc states metrics-side provides gate+signal, signer-side owns the synchronous cap + flag (no duplication)

<details><summary><b>Open questions</b> (6)</summary>

- Metric backend choice: Prometheus pull (assumed default to stay on the §10 $0-198/mo ladder) vs OTLP push to a managed collector. Spec names neither; confirm self-hosted Prometheus+Grafana is acceptable for Milestone 1.
- p_land cold-start prior value and min_samples threshold: spec gives ~90-99% revert (so p_land ~1-10%) as an estimate, not a committed prior. Need a chosen conservative default (e.g. p0=0.03) and the EWMA alpha.
- Exact numeric kill-switch thresholds beyond revert>30%: burn-rate lamports/min max, realized-loss SOL/hr max, and hot-key balance-deviation lamports are required to be numeric (§9) but values are unspecified - must be set against the chosen working-capital size.
- Golden-replay tolerance: sizing predicted_out should be 0-bps (bit-exact mirror), but net-after-fees may legitimately drift with live epoch fees - need an agreed tolerance split between sizing (strict) and net (looser).
- Where realized rug/honeypot loss feeds E[rug] in EconParams: is it a static configured prior for Milestone 1, or learned from the pre-trade vetting module's outcomes? Spec treats it as a model term without an estimator.
- Confirmation-rank source of truth: getBundleStatuses gives landed slot, but intra-block index may require block fetch - confirm whether index_in_block is captured live or backfilled from a block query (latency/cost tradeoff).

</details>

---

## 6. Master task DAG (107 tasks)

Flattened, cross-module dependencies resolved. `★` marks a critical-path task. Full acceptance criteria are in the per-module task lists (§5).

### Fase 0 — 20 tasks

| | Task | Module | Est | Depends on |
|---|---|---|---|---|
| ★ | `scaffold-1` Init git repo, monorepo skeleton, .gitignore secrets guard | scaffold | 0.5d | — |
| ★ | `scaffold-2` Pin toolchain: rust-toolchain.toml + versions.toml + bootstrap | scaffold | 1d | `scaffold-1` |
|  | `scaffold-6` Author infra/config TOMLs: program_ids, providers, limits | scaffold | 0.5d | `scaffold-1` |
| ★ | `scaffold-3` Cargo workspace + centralized pinned deps + committed lockfile | scaffold | 1.5d | `scaffold-2` |
| ★ | `scaffold-4` arb-config no_std core: program_ids + limits constants | scaffold | 1d | `scaffold-3` |
|  | `scaffold-5` arb-config std: providers/landing, secrets loader, loader+validate | scaffold | 1.5d | `scaffold-4`, `scaffold-6` |
|  | `scaffold-9` Supply-chain integrity: deny.toml, integrity-hashes, cargo-audit/deny | scaffold | 1d | `scaffold-3` |
|  | `scaffold-7` Config-consistency tooling: verify-config.sh + Solscan cross-check | scaffold | 0.5d | `scaffold-5`, `scaffold-6` |
|  | `scaffold-8` Key/program-keypair gen script + secrets contract enforcement | scaffold | 0.5d | `scaffold-5` |
|  | `scaffold-11` LiteSVM + Surfpool test substrate wiring + smoke test | scaffold | 1d | `scaffold-4` |
|  | `scaffold-10` CI pipeline: build/lint/test/lockfile/audit/config gates | scaffold | 1d | `scaffold-7`, `scaffold-9`, `scaffold-11` |
| ★ | `onchain-1` Crate scaffold + entrypoint + verifiable-build setup | onchain | 2d | `scaffold-4` |
| ★ | `sizing-1` Wide integer-math primitives: U256, mul_div, rounding | sizing | 1.5d | `scaffold-4` |
|  | `detection-1` Detection config + venue program-id verification | detection | 1.5d | `scaffold-5`, `scaffold-6` |
|  | `txbuilder-1` Module scaffold, config, hard-limit constants | txbuilder | 1.5d | `scaffold-4` |
|  | `txbuilder-2` ComputeBudget instruction builder + measured-CU sizing | txbuilder | 1d | `txbuilder-1` |
|  | `signer-1` Key security baseline + supply-chain hygiene gate | signer | 2d | `scaffold-1`, `scaffold-9` |
|  | `signer-2` SolanaSigner trait + MemorySigner hot-key backend | signer | 2d | `signer-1`, `scaffold-5` |
|  | `landing-1` Jito account, UUID and Sender baseline (Fase 0 setup seam) | landing | 2d | `scaffold-1` |
|  | `testing-1` Fase 0: toolchain + LiteSVM bootstrap + skeleton build | testing | 2d | `onchain-1`, `scaffold-11` |

### Fase 1 — 51 tasks

| | Task | Module | Est | Depends on |
|---|---|---|---|---|
|  | `scaffold-12` Verifiable/reproducible build pipeline (solana-verify) + Squads deploy… | scaffold | 1d | `scaffold-10` |
| ★ | `onchain-2` Error enum + instruction-data layout + Dex/LegDescriptor | onchain | 2d | `onchain-1` |
|  | `onchain-3` Pinned allowlist + trust-boundary verification | onchain | 2d | `onchain-1`, `onchain-2` |
|  | `onchain-4` Zero-copy balance read (state.rs) | onchain | 1d | `onchain-1` |
|  | `onchain-5` Token-2022 extension filter (token2022.rs) | onchain | 2d | `onchain-1`, `onchain-2` |
| ★ | `onchain-6` Raydium CPMM swap adapter | onchain | 3d | `onchain-3`, `onchain-4`, `onchain-5` |
|  | `onchain-7` Orca Whirlpool swap_v2 adapter | onchain | 3d | `onchain-3`, `onchain-4`, `onchain-5` |
| ★ | `onchain-8` Processor: snapshot->CPI A->delta->CPI B->terminal assert | onchain | 3d | `onchain-6`, `onchain-7` |
| ★ | `onchain-9` LiteSVM unit tests: revert, success, trust-boundary, CU | onchain | 3d | `onchain-8` |
| ★ | `onchain-10` Rounding-mirror fuzz/property gate (per-venue, both dirs) — MILESTONE-… | onchain | 4d | `onchain-9`, `sizing-8` |
|  | `onchain-11` Surfpool mainnet-fork integration test (revert on real programs) | onchain | 3d | `onchain-10` |
|  | `sizing-2` Token-2022 transfer-fee forward/inverse math | sizing | 1d | `sizing-1` |
| ★ | `sizing-3` Quoter trait + QuoteIn/Out/SwapDir/QuoteError + venue registry | sizing | 1d | `sizing-1`, `sizing-2` |
| ★ | `sizing-4` Raydium CP-Swap Quoter (bit-exact) | sizing | 1.5d | `sizing-3` |
|  | `sizing-5` Orca Whirlpool Quoter (bit-exact, in-range) | sizing | 2.5d | `sizing-3` |
|  | `sizing-6` PumpSwap AMM Quoter (bit-exact) | sizing | 1d | `sizing-3` |
| ★ | `sizing-7` RoundTrip composite + CpmmReserves extraction | sizing | 1d | `sizing-4`, `sizing-5`, `sizing-6` |
| ★ | `sizing-8` Closed-form delta* + opportunity predicate + policy (90-95%) | sizing | 2d | `sizing-7` |
|  | `sizing-9` GATE: per-venue both-direction differential/property test | sizing | 2.5d | `sizing-8`, `onchain-9` |
|  | `detection-2` Core model + SessionStamp dedupe types | detection | 1d | `detection-1` |
|  | `detection-3` Per-venue decoders (CPMM vaults+PoolState, Whirlpool, PumpSwap) | detection | 4d | `detection-2` |
|  | `detection-4` Idempotent pool-state cache + CPMM multi-component assembly | detection | 3d | `detection-3` |
|  | `detection-5` Yellowstone gRPC ingest client | detection | 2.5d | `detection-2` |
|  | `detection-6` Token-pair graph + incremental edge recompute | detection | 2d | `detection-4` |
|  | `detection-7` Reconnect/replay supervisor + run-loop wiring | detection | 3d | `detection-4`, `detection-5`, `detection-6` |
|  | `txbuilder-3` Token-2022 HARD-REJECT extension filter | txbuilder | 2.5d | `txbuilder-1` |
|  | `txbuilder-4` WSOL dance helper | txbuilder | 1.5d | `txbuilder-1` |
|  | `txbuilder-8` ALT manager: create/extend/resolve append-only + ~30 chunking | txbuilder | 2.5d | `txbuilder-1` |
|  | `txbuilder-9` ALT warm-up gate + static-table set | txbuilder | 2d | `txbuilder-8` |
|  | `txbuilder-10` ALT janitor (async close after ~512 slots) | txbuilder | 1.5d | `txbuilder-8` |
| ★ | `txbuilder-5` Canonical instruction layout + core message assembler | txbuilder | 2.5d | `txbuilder-2`, `txbuilder-4`, `txbuilder-9`, `onchain-8`, `sizing-8` |
|  | `txbuilder-6` Hard-limit validation gate (locks<128, bytes<=1232, CU<1.4M) | txbuilder | 2d | `txbuilder-5` |
|  | `txbuilder-7` Preflight simulateTransaction wrapper + profit-check | txbuilder | 2.5d | `txbuilder-5`, `txbuilder-2` |
|  | `txbuilder-11` Pre-build route vetting (filter + DEX allowlist mirror + frozen-ATA) | txbuilder | 1.5d | `txbuilder-3`, `txbuilder-4` |
|  | `txbuilder-12` End-to-end build harness on mainnet-fork (LiteSVM/Surfpool) | txbuilder | 2.5d | `txbuilder-6`, `txbuilder-7`, `txbuilder-9` |
|  | `signer-3` TxShapeValidator (allowlist, dest=own-ATA, max-lamport-out, tip) | signer | 3d | `signer-2`, `onchain-8` |
|  | `signer-4` Synchronous PreSignCaps (count + cumulative lamport-out) | signer | 2d | `signer-2` |
|  | `signer-5` SignerSidecar canonical sign path (flag->shape->caps->sign, atomic) | signer | 2d | `signer-3`, `signer-4` |
|  | `testing-2` Pool + mint builders for LiteSVM | testing | 4d | `testing-1`, `sizing-7` |
|  | `testing-3` SwapHarness test program + single-leg client | testing | 3d | `testing-2` |
|  | `testing-4` LiteSVM unit tests: revert + exact-delta + boundary | testing | 3d | `testing-2`, `testing-3`, `sizing-4`, `onchain-8` |
| ★ | `testing-5` MILESTONE-1 GATE: differential/property rounding-mirror test | testing | 5d | `testing-3`, `testing-2`, `sizing-8`, `onchain-10` |
|  | `testing-6` Trust-boundary + Token-2022 filter negative tests | testing | 3d | `testing-2`, `testing-4`, `onchain-3`, `onchain-5` |
|  | `testing-7` CU / account-lock / tx-byte budget + ALT pre-warm asserts | testing | 2d | `testing-4`, `txbuilder-9` |
|  | `testing-8` Surfpool mainnet-fork integration vs real Raydium/Orca | testing | 5d | `testing-4`, `testing-6`, `txbuilder-12` |
|  | `testing-9` Deterministic historical replay (Yellowstone / Old Faithful) | testing | 3d | `testing-5`, `testing-8` |
|  | `observ-1` Metric registry + canonical keys (lock-free hot path) | observability | 2d | `scaffold-3` |
|  | `observ-4` Probabilistic cost model + synchronous cost-gate | observability | 2.5d | `scaffold-3`, `sizing-8` |
|  | `observ-10` Golden-replay corpus format + loader | observability | 1.5d | `scaffold-3` |
|  | `observ-2` Latency spans (P50/P95) + confirmation-rank capture | observability | 1.5d | `observ-1` |
|  | `observ-3` PnL ledger + burn-rate accumulator | observability | 2d | `observ-1` |

### Fase 2 — 31 tasks

| | Task | Module | Est | Depends on |
|---|---|---|---|---|
|  | `onchain-12` PumpSwap AMM adapter (Fase 2 venue) | onchain | 3d | `onchain-8`, `onchain-10` |
|  | `onchain-13` Deploy upgradeable + publish verifiable build (Squads authority) | onchain | 2d | `onchain-11`, `scaffold-12` |
|  | `detection-8` Detection metrics + latency instrumentation | detection | 1.5d | `detection-7`, `observ-2` |
|  | `detection-9` Fase-2 targeted subscription sizing (20-50 pairs) + PumpSwap integrati… | detection | 2.5d | `detection-3`, `detection-7` |
|  | `txbuilder-13` Jito tip instruction (Fase 2 seam) + tip capping | txbuilder | 1.5d | `txbuilder-5` |
|  | `txbuilder-14` PumpSwap AMM venue support in builder/vet (Fase 2) | txbuilder | 1.5d | `txbuilder-11` |
|  | `signer-6` KillSwitch flag + handle (manual halt < seconds, no auto re-arm) | signer | 2d | `signer-5` |
|  | `signer-7` KillSwitchSupervisor + numeric thresholds + alert routing | signer | 3d | `signer-6`, `observ-6` |
|  | `signer-8` Blast-radius sweeper (cron + threshold) to cold treasury | signer | 3d | `signer-7` |
|  | `signer-9` Hot-key rotation + working-capital funding ops | signer | 2d | `signer-8` |
|  | `signer-10` Deploy posture: Squads upgrade authority + solana-verify reproducible… | signer | 3d | `signer-1`, `scaffold-12` |
|  | `signer-11` Kill-switch recovery runbook + manual-halt drill + on-call posture | signer | 2d | `signer-7`, `signer-9` |
|  | `signer-12` End-to-end signer integration test on mainnet-fork (Surfpool) | signer | 3d | `signer-8`, `signer-10` |
| ★ | `landing-2` JitoClient JSON-RPC + regional fan-out + rate limiter | landing | 4d | `landing-1`, `txbuilder-5` |
| ★ | `landing-3` TipOracle: tip_floor REST + tip_stream WS, sizing + load-balance | landing | 3d | `landing-2`, `txbuilder-13` |
| ★ | `landing-4` Bundle build: tip-inside-atomic-tx + jitodontfront + hard-limit guard | landing | 4d | `landing-3`, `txbuilder-6` |
| ★ | `landing-5` Pre-tip simulation gate (simulateTransaction / simulateBundle) | landing | 3d | `landing-4`, `txbuilder-7` |
| ★ | `landing-6` Strict landing loop with fresh-blockhash rebuild | landing | 4d | `landing-5` |
| ★ | `landing-7` Helius Sender fallback + SWQoS non-bundle + routing-exclusivity guard | landing | 3d | `landing-6` |
| ★ | `landing-8` Executor facade, route selection, signer handshake | landing | 3d | `landing-7`, `signer-5` |
| ★ | `landing-9` Executor metrics: revert-rate, burn-rate, latency, drop-cause | landing | 2d | `landing-8`, `observ-1` |
|  | `testing-10` Fase-2 forward hook: PumpSwap differential + Surfpool clone | testing | 3d | `testing-5`, `testing-8`, `onchain-12`, `sizing-6` |
|  | `observ-5` p_land EWMA estimator (per route + tip bucket) | observability | 2d | `observ-4`, `observ-3` |
|  | `observ-6` Health evaluator + numeric kill-switch thresholds | observability | 2d | `observ-2`, `observ-3` |
|  | `observ-7` Prometheus exporter + /healthz (off hot path) | observability | 1.5d | `observ-1`, `observ-6` |
|  | `observ-8` Deviation-alert router (Telegram/PagerDuty) + runbook links | observability | 1.5d | `observ-6` |
|  | `observ-9` Realized-slippage-per-route instrumentation | observability | 1d | `observ-1` |
|  | `observ-11` Golden-replay regression gate (predicted vs realized) — CI-blocking | observability | 2.5d | `observ-10`, `observ-4` |
|  | `observ-12` Aggregate backtest + unit-economics confirmation report | observability | 1.5d | `observ-10`, `observ-4`, `observ-5` |
|  | `observ-13` Grafana dashboard + deviation alert rules | observability | 1d | `observ-7`, `observ-9` |
|  | `observ-14` Wire cost-gate into signer pre-sign + health into kill-switch (integra… | observability | 1.5d | `observ-4`, `observ-6`, `signer-5`, `signer-7` |

### Fase 3 — 5 tasks

| | Task | Module | Est | Depends on |
|---|---|---|---|---|
|  | `onchain-14` FORWARD SEAM: PDA-vault / invoke_signed abstraction hook (Fase 3) | onchain | 1d | `onchain-8` |
|  | `sizing-10` FASE-3 SEAM: golden-section search + Bellman-Ford cycle (gated) | sizing | 2d | `sizing-8` |
|  | `detection-10` FASE 3 forward-hook: owner-firehose discovery seam | detection | 1d | `detection-4` |
|  | `signer-13` FORWARD HOOK: KMS/Fireblocks treasury backend seam (Fase 3+) | signer | 1d | `signer-2` |
|  | `landing-10` Durable-nonce forward seam (Fase 3 hook, design-only for M1) | landing | 1d | `landing-6` |

---

## 7. Critical path & integration milestones

**Critical path to first profitable mainnet land** (longest dependency chain):

`scaffold-1` → `scaffold-2` → `scaffold-3` → `scaffold-4` → `sizing-1` → `sizing-3` → `sizing-4` → `sizing-7` → `sizing-8` → `onchain-1` → `onchain-2` → `onchain-6` → `onchain-8` → `onchain-9` → `onchain-10` → `testing-5` → `txbuilder-5` → `landing-2` → `landing-3` → `landing-4` → `landing-5` → `landing-6` → `landing-7` → `landing-8` → `landing-9`

**Integration milestones** (the go/no-go checkpoints):

- **M0: Workspace foundation green** — cargo workspace builds with committed lockfile + pinned deps; arb-config (no_std program_ids + limits) and arb-types consumable by onchain and bot; all Wave-1 program IDs verified on Solscan and pinned with onchain/allowlist.rs authoritative + infra/config mirror; supply-chain gate (cargo-audit/deny + integrity hashes) green in CI.
  - Depends on: `scaffold-3`, `scaffold-4`, `scaffold-7`, `scaffold-9`, `scaffold-10`
- **M0.5: Skeleton builds + LiteSVM revert proven** — onchain TryArbitrage skeleton builds; LiteSVM smoke + first revert path (FailedTransactionMetadata on no-arb) demonstrated; Surfpool can clone Raydium CPMM + Orca Whirlpool pools.
  - Depends on: `onchain-1`, `onchain-8`, `onchain-9`, `scaffold-11`, `testing-1`
- **M1-GATE: Rounding-mirror fuzz/property gate GREEN** — THE Milestone-1 gate. Off-chain sizing predicted_out == on-chain CPI realized_out, proven by fuzz/property test across wide (reserves, fees, amount_in), BOTH directions, BOTH DEX (Raydium CP-Swap + Orca Whirlpool), incl Token-2022 fee path. Reconciles the three overlapping gate tasks: onchain-10 (on-chain side harness+fuzz), sizing-9 (off-chain differential), testing-5 (canonical cross-module gate…
  - Depends on: `sizing-8`, `onchain-9`, `onchain-10`, `sizing-9`, `testing-5`
- **M1.5: Atomic tx builds + reverts on real programs (mainnet-fork)** — v0 VersionedTransaction with pre-warmed ALT (no extend-then-use same slot), WSOL dance, Token-2022 HARD-REJECT filter, hard-limit validation (locks<128, bytes<=1232, CU<1.4M); end-to-end build harness reverts on intentionally-unprofitable input against real Raydium/Orca on Surfpool.
  - Depends on: `txbuilder-5`, `txbuilder-6`, `txbuilder-9`, `txbuilder-12`, `onchain-11`, `testing-8`
- **M2: First profitable mainnet land via Jito** — Land profitable >=1 time on mainnet at small size via Jito 1-tx bundle with tip INSIDE the atomic tx; landing loop with status poll + fresh-blockhash rebuild; signer kill-switch + sweeper operational; observability (revert-rate, burn-rate, latency, PnL) live; cost-gate wired into pre-sign and health into kill-switch.
  - Depends on: `landing-4`, `landing-6`, `landing-8`, `landing-9`, `signer-8`, `observ-14`
- **M2.5: Deploy posture + verifiable build published** — Program deployed upgradeable with Squads multisig upgrade authority; verifiable/reproducible build published via solana-verify; deploy + kill-switch-recovery runbooks written and drilled.
  - Depends on: `scaffold-12`, `onchain-13`, `signer-10`, `signer-11`

> **`M1-GATE` is the single hard go/no-go.** It reconciles the three overlapping gate tasks (`onchain-10` on-chain harness+fuzz, `sizing-9` off-chain differential, `testing-5` canonical cross-module gate) into **one** work-item with one owner (see §9 sequencing fix). No mainnet capital before it is green.

---

## 8. Phase sequencing

Durations below reconcile `plan.md` §11's headline numbers against the summed task estimates. **The re-estimates (right column) are the planning numbers** — §11's headlines materially undershoot once all nine module tracks are accounted for. Phases gate on exit-criteria, not the calendar.

| Phase | `plan.md` §11 | **Realistic (plan to this)** |
|---|---|---|
| Fase 0 | 1–2 weeks | **2.5–3.5 weeks** (critical subset `scaffold-1..4 + onchain-1 + sizing-1` ≈ 8 days; rest parallelizable) |
| Fase 1 | 3–5 weeks | **6–7 weeks** (critical sub-chain sizing+onchain+`M1-GATE` ≈ 30–34 dev-days even with everything else parallel) |
| Fase 2 | 3–4 weeks | **5–6 weeks** (landing chain `landing-2..9` ≈ 26 dev-days serial; §10 itself flags this as the most-likely-to-slip phase) |
| Fase 3 | 4–6 weeks | seams only in M1 (~6 dev-days); full venue/triangular/flash-loan build is post-M1 |
| Fase 4 | ongoing | not in the M1 task list; golden-replay gate from Fase 2 carries forward |

### Fase 0 — Dev environment ready, toolchain pinned, all program IDs verified on-chain, workspace + shared crates (arb-config/arb-types) compile, LiteSVM/Surfpool substrate wired, key-security and supply-chain baseline, accounts provisioned (Jito ShredStream allowlist, free WSS, Helius Sender, Jito UUID). This is the shared foundation every downstream module links.

**Entry:**
- Canonical plan.md approved
- Empty repo / greenfield

**Exit:**
- cargo workspace builds with committed Cargo.lock + pinned [workspace.dependencies]
- arb-config no_std core (program_ids + limits) and arb-types compile and are consumable by onchain + bot
- All Wave-1 program IDs (Raydium CPMM, Orca Whirlpool, PumpSwap AMM) verified on Solscan and pinned in infra/config + onchain/allowlist.rs (mirrors)
- onchain skeleton (buffalojoec-style) builds and LiteSVM smoke test green
- cargo-audit/cargo-deny + integrity-hashes gate green in CI
- Key security baseline: hot keypair chmod 600, not in git, deps pinned+lockfile+integrity hash
- Surfpool can clone Raydium CPMM + Orca Whirlpool pools; Jito UUID + getTipAccounts resolved at runtime

**Tasks:** `scaffold-1`, `scaffold-2`, `scaffold-3`, `scaffold-4`, `scaffold-5`, `scaffold-6`, `scaffold-7`, `scaffold-8`, `scaffold-9`, `scaffold-10`, `scaffold-11`, `onchain-1`, `sizing-1`, `detection-1`, `txbuilder-1`, `txbuilder-2`, `signer-1`, `signer-2`, `landing-1`, `testing-1`

> _Duration note:_ plan §11 = 1-2 weeks. Summed Fase-0 task estimates ~26.5 dev-days (~5.3 wk for one dev). DISAGREEMENT: §11's 1-2 weeks assumes the onchain/sizing skeleton stubs only; the full set of Fase-0 deliverables across 9 modules (config std side, supply-chain gate, signer baseline, Jito setup, LiteSVM bootstrap) realistically takes 2.5-3.5 weeks for a solo dev. Treat 1-2 wk as the critical-path subset (scaffold-1..4 + onchain-1 + sizing-1 = ~8 days), the rest parallelizable.

### Fase 1 — On-chain TryArbitrage (2 swap CPI Raydium CPMM + Orca Whirlpool + terminal profit-assert) proven to revert on unprofitable input, with off-chain sizing engine bit-exact-mirroring on-chain integer math, validated by the per-venue both-direction rounding-mirror fuzz/property gate (Milestone-1 gate) on LiteSVM and Surfpool mainnet-fork. v0 tx + pre-warmed ALT, WSOL dance, Token-2022 HARD-REJECT filter, detection cache idempotent.

**Entry:**
- Fase 0 exit criteria met
- arb-config/arb-types stable; onchain skeleton builds; sizing math primitives (sizing-1) ready
- LiteSVM substrate green

**Exit:**
- TryArbitrage reverts Unprofitable on no-arb input (LiteSVM FailedTransactionMetadata) AND succeeds with exact predicted delta on profitable input
- MILESTONE-1 GATE GREEN: predicted_out == on-chain realized_out via fuzz/property test, BOTH directions, BOTH DEX (Raydium CP-Swap + Orca Whirlpool), incl Token-2022 fee path (onchain-10 + sizing-9 + testing-5 reconciled as one gate)
- Trust boundary: program rejects swap-CPI to non-allowlist program + balance-read from non-bot account
- CU per leg measured; total tx <1.4M CU; account locks <128; tx <=1232 bytes; ALT pre-warmed >=1 slot (no extend-then-use same slot)
- Token-2022 filter rejects hook/frozen/non-transferable/memo/confidential/permanent-delegate + mint-close-authority
- WSOL wrap->sync->close complete, no stuck ATA; detection cache idempotent (slot+write_version intra-session, reconnect prefers higher slot)
- Revert validated with intentionally-unprofitable input on Surfpool mainnet-fork

**Tasks:** `scaffold-12`, `onchain-2`, `onchain-3`, `onchain-4`, `onchain-5`, `onchain-6`, `onchain-7`, `onchain-8`, `onchain-9`, `onchain-10`, `onchain-11`, `sizing-2`, `sizing-3`, `sizing-4`, `sizing-5`, `sizing-6`, `sizing-7`, `sizing-8`, `sizing-9`, `detection-2`, `detection-3`, `detection-4`, `detection-5`, `detection-6`, `detection-7`, `txbuilder-3`, `txbuilder-4`, `txbuilder-5`, `txbuilder-6`, `txbuilder-7`, `txbuilder-8`, `txbuilder-9`, `txbuilder-10`, `txbuilder-11`, `txbuilder-12`, `signer-3`, `signer-4`, `signer-5`, `testing-2`, `testing-3`, `testing-4`, `testing-5`, `testing-6`, `testing-7`, `testing-8`, `testing-9`, `observ-1`, `observ-2`, `observ-3`, `observ-4`, `observ-10`

> _Duration note:_ plan §11 = 3-5 weeks. Summed Fase-1 task estimates ~108 dev-days (~21 wk solo, sequential). DISAGREEMENT (significant): §11's 3-5 wk is only realistic with parallelism across the onchain/sizing/detection/txbuilder/testing/observ tracks AND treating the rounding-mirror gate as the single hard milestone. Critical sub-chain (sizing-1->...->sizing-8 + onchain-1->...->onchain-10 + testing-5) is ~30-34 dev-days (~6-7 wk) even with everything else parallel. Recommend planning Fase 1 at 6-7 weeks, not 3…

### Fase 2 — Land profitable on mainnet (small size) via Jito 1-tx bundle with tip INSIDE the atomic tx, a correct landing loop (status poll + fresh-blockhash rebuild), Helius Sender/SWQoS fallback with routing exclusivity, signer kill-switch + sweeper operational, PumpSwap AMM added as venue, and full observability (revert-rate, burn-rate, latency P50/P95, PnL, realized slippage).

**Entry:**
- Fase 1 exit criteria met (Milestone-1 gate green)
- Chainstack Yellowstone gRPC endpoint provisioned (~20-50 targeted pairs)
- Jito allowlisted UUID active; getTipAccounts resolved
- Signer canonical sign path (signer-5) and txbuilder message assembler (txbuilder-5) ready

**Exit:**
- Landed PROFITABLE >=1 time on mainnet at small size via Jito 1-tx bundle with tip inside atomic tx (fail => tip unpaid)
- Tip accounts resolved via getTipAccounts at runtime (not hardcoded); jitodontfront stamped; routing exclusive via Jito (no silent non-protected fallback)
- Tip sized dynamically from tip_floor and capped as fraction of simulated profit
- Landing loop: poll getInflightBundleStatuses->getBundleStatuses, rebuild fresh blockhash on no-land (no blockhash reuse); simulateBundle passes before tip
- Helius Sender fallback (skipPreflight=true, maxRetries=0) wired
- PumpSwap AMM integrated as venue (onchain-12 + sizing-6 + txbuilder-14 + detection-9)
- Signer sidecar isolates small-balance hot key; treasury in KMS/Squads multisig; kill-switch manual halt <seconds + auto-trip on revert-rate/loss; sweeper moves surplus to cold treasury
- Dashboard live (revert-rate/burn-rate/latency/PnL); deviation alerts active; cost-gate wired into pre-sign, health into kill-switch (observ-14)
- Program deployed upgradeable with Squads upgrade authority + published verifiable build (solana-verify)

**Tasks:** `onchain-12`, `onchain-13`, `detection-8`, `detection-9`, `txbuilder-13`, `txbuilder-14`, `signer-6`, `signer-7`, `signer-8`, `signer-9`, `signer-10`, `signer-11`, `signer-12`, `landing-2`, `landing-3`, `landing-4`, `landing-5`, `landing-6`, `landing-7`, `landing-8`, `landing-9`, `testing-10`, `observ-5`, `observ-6`, `observ-7`, `observ-8`, `observ-9`, `observ-11`, `observ-12`, `observ-13`, `observ-14`

> _Duration note:_ plan §11 = 3-4 weeks; §10 caveat explicitly flags this as the phase most likely to slip and revises to 4-6 weeks. Summed Fase-2 task estimates ~80 dev-days (~16 wk solo). DISAGREEMENT: trust the §10 caveat (4-6 wk) over the §11 headline (3-4 wk). Landing chain (landing-2->...->landing-9) is ~26 dev-days serial (~5-6 wk) and is the critical path to first mainnet land; plan Fase 2 at 5-6 weeks with explicit contingency buffer.

### Fase 3 — Expand surface to more venues (Raydium AMM v4 V2-8acct, Meteora DLMM/DAMM v2, Phoenix CLOB) and 3-leg triangular routes; optional flash loan (marginfi top-level assembly) for size > inventory; CLMM/DLMM golden-section sizing; durable-nonce + KMS/Fireblocks + owner-firehose discovery seams activated.

**Entry:**
- Fase 2 exit criteria met (profitable mainnet land + operational kill-switch/sweeper)
- Fase-3 forward seams present and compile-gated (onchain-14, sizing-10, detection-10, landing-10, signer-13)

**Exit:**
- Raydium v4 (8-acct V2), Meteora DLMM/DAMM v2, Phoenix integrated and tested on fork
- CLMM/DLMM ternary/golden-section sizing matches on-chain within tolerance
- Triangular discovery (Bellman-Ford negative-cycle) + per-leg re-sizing; returns-to-start-token
- Optional flash-loan top-level assembly with on-chain fee-reserve read + profit-after-fee gate
- Account locks <128 & tx <=1232 bytes maintained on multi-hop routes via ALT; janitor reclaims rent

**Tasks:** `onchain-14`, `sizing-10`, `detection-10`, `landing-10`, `signer-13`

> _Duration note:_ plan §11 = 4-6 weeks (forward seams in M1 are ~6 dev-days; full Fase-3 venue/triangular/flash-loan build is the bulk and is out-of-scope for the M1 task list here).

### Fase 4 — Competitive latency edge: CU minimization (nostd entrypoint, noalloc, bitwise parse), ShredStream co-location, regional fan-out, adaptive tip model, BAM-readiness abstraction, golden-replay regression gate as deploy-capital gate.

**Entry:**
- Fase 3 multi-venue surface stable
- Sustained niche profitability instrumented

**Exit:**
- CU per leg optimized + precise SetComputeUnitLimit (no overpay)
- ShredStream active (allowlisted) + co-located near Jito region; staked RPC for non-bundle path
- Adaptive tip model on acceptance-rate; tip/ordering interface migration-ready to BAM
- Golden-replay corpus enforced as regression gate (predicted==realized within tolerance)

> _Duration note:_ plan §11 = ongoing (no fixed end; not part of M1 task list — observ-11/observ-12 golden-replay gate built in Fase 2 carries forward as the Fase-4 regression gate).

---

## 9. [critique] Addendum — tasks & decisions required before mainnet capital

The adversarial completeness pass found that the module DAG, while covering the large majority of the spec's Definition of Done, leaves a handful of **load-bearing** items either unowned or only mentioned in risk-prose. These are not optional: items (1)–(4) block a *safe* first mainnet land for the exact hot-pool / fresh-launchpad niche this system targets. **Add them before committing capital.**

### 9.1 Added tasks

#### `add-1` · **BLOCKER** · Fase 2 · owner: executor (+ detection hook)
**In-flight writable-account registry + concurrent-opportunity dedupe (one-inflight-per-pool)**

_Why:_ `plan.md` §6 marks this KRITIS. Multiple concurrent opportunities on the same hot pool take the same writable lock → they serialize/collide, waste tips+fees, and defeat Jito's parallel auction (which only parallelizes on disjoint locks). This is the dominant self-collision mode for the niche.

_Done when:_
- Two opportunities touching the same writable pool account cannot both be in-flight; the second is gated/dropped with a distinct `DropCause::WritableContention`.
- Dedupe is keyed on pool pubkey at the detection→executor boundary.
- A drop-cause metric distinguishes lock-contention from tip-loss.
- The registry check runs in `Executor::land` before signing.

#### `add-2` · **HIGH** · Fase 1 · owner: onchain + signer + testing
**Inventory round-trip-closure invariant (leg-B output mint == leg-A input/base mint)**

_Why:_ `plan.md` §6 Milestone-1 inventory invariant — the property that makes M1 inventory-safe (no WSOL↔USDC drift). A mis-resolved second leg could strand the intermediate asset yet still pass a profit-assert read on the wrong mint.

_Done when:_
- On-chain processor asserts the base ATA mint of leg-A input equals the profit-checked mint of leg-B output (round-trip closure) — or `TryArbitrageData` pins a single base mint both legs reference.
- LiteSVM negative test: a non-returning route reverts.
- `TxShapeValidator` (signer-3) asserts the route closes to the owned base ATA.

#### `add-3` · **HIGH** · Fase 2 · owner: txbuilder / pre-trade vetting
**Route-specific SELL-simulation honeypot/rug gate (non-Jupiter)**

_Why:_ `plan.md` §9 requires honeypot vetting that does NOT gate on Jupiter (fresh launchpad pools are unindexed = the niche). The whole plan targets the high-rug/honeypot niche, so this is load-bearing; observ's E[rug/honeypot] term currently has no feeder.

_Done when:_
- Simulate a SELL against the exact pool to be routed (not Jupiter); a honeypot/no-exit pool is rejected pre-sign.
- Outcome feeds observ's E[rug/honeypot] term.
- Treated as necessary-not-sufficient; the on-chain assert remains the final net.

#### `add-4` · **HIGH** · Fase 1 · owner: detection (on-demand RPC) / txbuilder
**Live per-epoch Token-2022 fee read (`getEpochFee`) with epoch-boundary refetch**

_Why:_ `plan.md` §9 mandates live per-tx fee reads, no cross-epoch caching. `sizing-2` only ENFORCES staleness (`EpochFeeStale`) — nothing FETCHES a fresh `TransferFeeConfig` per opportunity, so the guard is never satisfied by a real producer. A stale/missing fee yields a wrong net_out → predicted-profitable trade reverts.

_Done when:_
- `TransferFeeConfig.epoch == current epoch` is fetched per opportunity for each Token-2022 mint in the route.
- An epoch boundary forces a refetch.
- A stale config surfaces as a hard reject, not a silent wrong quote.

#### `add-5` · **MEDIUM** · Fase 1 · owner: onchain
**Runtime SIMD-0268/0339 feature-gate detection + measured CU-per-CPI budget**

_Why:_ `plan.md` §6 explicitly says read the feature-gate at runtime — do not hardcode pre/post CPI-depth (5 vs 9) or max CPI account-infos (128 vs 255). Bounds the Fase-1 account/CPI budget and all Fase-3 multi-hop routing.

_Done when:_
- Program/bot reads activation state at runtime and selects CPI-depth/account-info budget accordingly.
- LiteSVM/Surfpool measures actual CU/CPI; no pre/post values are hardcoded.

#### `add-6` · **MEDIUM** · Fase 1 · owner: detection / txbuilder route-resolution
**Whirlpool tick-array / oracle on-demand resolver (quote + CPI account list)**

_Why:_ `plan.md` §5: tick/bin arrays are PDAs, fetched on-demand (+0 subscriptions). The in-range Whirlpool quote and the on-chain `swap_v2` CPI both need the correct current tick-array PDAs resolved consistently.

_Done when:_
- The in-range single-tick-array case resolves the correct tick-array PDAs for BOTH the off-chain quote and the on-chain `remaining_accounts`.
- The >1-tick-crossing fallback is documented as Fase 3 (sizing returns `CrossesTick` for M1).

#### `add-7` · **SEAM** · Fase 3 · owner: sizing (Quoter seam) + onchain adapter seam
**Phoenix CLOB partial-fill / IOC-FOK handling (forward-seam contract only)**

_Why:_ `plan.md` §11 Fase 3 marks Phoenix partial-fill 'wajib'. Out of the M1 task list, but reserve the contract now (IOC/FOK limit-price, ladder revalidation) so it can be added without redesigning the round-trip/sizing interfaces.

_Done when:_
- The `Quoter` trait + on-chain adapter expose a seam for fill-or-cancel semantics and ladder revalidation, with no change required to the M1 round-trip interface to add Phoenix later.

### 9.2 Decisions to close (contradictions found across module designs)

#### `dec-1` · Fase 0 · Single concrete Agave / Yellowstone / SIMD pin
_Issue:_ Three modules named three different Agave major lines (1.18.x / 2.1.0 / 3.x). The workspace `[workspace.dependencies]` (`crates/arb-config`) is the authoritative pin and the on-chain program links it `no_std`; a 3.x vs 2.1.0 mismatch will not even compile, and the runtime target also drives SIMD-0268/0339 feature-gate behavior.

_Resolution:_ Force ONE concrete Agave pin in `scaffold-3`'s `[workspace.dependencies]`; `onchain`/`txbuilder`/`executor` inherit via `workspace = true`. Choose against the actual mainnet runtime version at the planned deploy window (this also fixes the `add-5` feature-gate budget). Delete the per-module '3.x'/'1.18.x' wording. **Close in Fase 0.**

#### `dec-2` · Fase 2 · CapReservation lifecycle across the landing rebuild loop
_Issue:_ The landing loop rebuilds with a fresh blockhash and re-signs on each ~2.5 s no-land; each re-sign calls `PreSignCaps::reserve` again. Without a defined lifecycle, a rebuild-resign of the SAME opportunity either double-counts against the per-window cap (starving it) or leaks budget (reservation never released).

_Resolution:_ Define it explicitly: a rebuild-resign of the same opportunity RELEASES the prior reservation before reserving anew (same `window_epoch` restores count+lamport), OR carries one reservation handle through rebuilds so one opportunity consumes one count slot. Add acceptance to `signer-4`/`landing-6`: N rebuilds of one opportunity consume exactly one count (or a documented, intended N).

#### `dec-3` · Fase 1 · Single `min_profit` definition (base==WSOL asymmetry)
_Issue:_ Base/priority/tip are SOL-lamport costs not visible as a base-asset (WSOL/USDC) balance delta, so off-chain must bake them into `min_profit`. When base==WSOL the lamport tip/fee and the WSOL balance delta are the same asset, but the on-chain assert reads only the WSOL ATA amount, not the signer's native-SOL lamport delta. sizing's `predicted_profit` and the on-chain assert must agree on the EXACT same `min_profit` or a profitable-looking trade reverts.

_Resolution:_ Pin one definition in `arb-types`/shared docs: `min_profit` (base-asset terms) = swap_fees + priority + base + tip + margin, computed ONCE off-chain and used bit-identically by sizing, the tx-builder's profit expectation, and the on-chain assert. For base==WSOL, decide now whether to additionally read the signer lamport delta on-chain (tighter, more accounts/CU) or rely on client `min_profit`; record the decision so the modules cannot diverge.

### 9.3 Sequencing fix — collapse the M1-GATE cluster

`onchain-10`, `sizing-9`, and `testing-5` form a near-cyclic cluster (the on-chain harness, the off-chain differential, and the canonical cross-module gate test all describe the *same* rounding-mirror property). As written, the DAG edges risk being scheduled as three separate sequential gates and could deadlock on build order. **Collapse them into one shared `M1-GATE` work-item with a single owner:** the LiteSVM CPI harness that exports `realized_out(pool, amount_in, dir)` is built **once** and consumed by both the off-chain and cross-module sides. Enforce `M1-GATE green` as a hard Fase-2 entry gate (the DAG has no direct `landing-8 → testing-5` edge, so add it as a release rule).

### 9.4 Other review notes (lower severity)

- **Profit-assert account-budget accounting made explicit** (medium) — Extend txbuilder-6 / onchain-9 acceptance to itemize the profit-assert balance-read accounts in LimitReport (loaded-account contribution + marginal deser CU), asserting they are the already-swap-loaded ATAs (marginal) not net-new loads.
- **jitodontfront index-0 ordering enforcement** (low) — Add to landing-4 acceptance: the jitodontfront-stamped tx is asserted at bundle index 0; a test that a non-index-0 placement is rejected pre-send (guards future multi-tx bundles).
- **Confirmation-rank (slot,index_in_block) capture source** (low) — Pin the source in observ-2/landing-9 acceptance: index_in_block is either parsed from getBundleStatuses if available or backfilled via a getBlock query off the hot path, with a documented latency/cost tradeoff; assert it is populated, not defaulted.

---

## 10. Cross-cutting risk register

| Risk | Sev | Mitigation | Affected tasks |
|---|---|---|---|
| Rounding-mirror divergence: off-chain integer math (Floor output / Ceil required-input) fails to bit-match on-chain realized_out for any (venue, direction), so live txs predicted profitable revert on-chain — burning CU+f… | **critical** | Make the per-venue both-direction fuzz/property gate (onchain-10 + sizing-9 + testing-5, reconciled as ONE Milestone-1 gate) a hard CI-blocking go/no-go before ANY mainnet send. Implement quoters bit-exact against canonical source (raydium-cp-swap, whirlpool), cross-check Token-2022 fee against spl-… | `sizing-4`, `sizing-5`, `sizing-6`, `sizing-8`, `sizing-9`, `onchain-6`, `onchain-7`, `onchain-8`, `onchain-10`, `testing-5`, `observ-11` |
| ALT warm-up timing: extending an ALT (incl create+extend) and using those addresses in the same slot causes v0 key-resolution failure — the tx silently fails to land. New pools (the niche) constantly need ALT extension o… | **high** | warmup::is_warm gate (last_extended_slot < current_slot) enforced before any route uses ALT-resolved keys; long-lived STATIC table for invariant accounts warmed far ahead; per-route tables spun off-path; janitor closes async after ~512 slots. Test assert in testing-7 that no extend-then-use occurs s… | `txbuilder-8`, `txbuilder-9`, `txbuilder-10`, `txbuilder-5`, `testing-7`, `detection-9` |
| Writable-lock self-contention: many opportunities on the SAME pool take a writable lock on the same pool account -> txs serialize and collide; in Jito the parallel auction only runs when locks are disjoint, so concurrent… | **high** | Mutex one-inflight-per-writable-account in the executor + dedupe concurrent opportunities touching the same pool (detection emits per-pool, executor gates). Track drop-cause to distinguish lock-contention from tip-loss. Per-writable-account 12M CU/block ceiling acknowledged in budgeting. | `landing-4`, `landing-6`, `landing-8`, `detection-6`, `detection-7`, `txbuilder-6` |
| write_version cross-session incomparability: write_version is global-monotonic per-validator/per-session and NOT comparable across reconnect / multi-node failover (e.g. LaserStream node switch). Naive dedupe drops fresh… | **high** | Cache accept predicate: within same session_epoch compare (slot, then write_version); across session_epoch prefer higher slot UNCONDITIONALLY. On reconnect resubscribe from_slot=last_processed and treat first post-reconnect update as authoritative by slot. Property test cache ordering + reconnect in… | `detection-2`, `detection-4`, `detection-5`, `detection-7` |
| Hot-key compromise / supply-chain malware: hot key is loss-vector #1. Documented wallet-draining 'Solana-Arbitrage-Bot' clusters scrape PRIVATE_KEY from .env and exfil; @solana/web3.js 1.95.6/7 trojan (CVE-2024-54134). A… | **critical** | Hot key small-balance only, in-memory (Zeroizing), never in git, chmod 600; treasury/upgrade authority in KMS + Squads multisig. Pin deps + committed lockfile + integrity hashes + cargo-audit/deny CI gate; never run untrusted bot repos with funded keys; sandbox + throwaway keypair. Async sweeper cap… | `scaffold-1`, `scaffold-8`, `scaffold-9`, `signer-1`, `signer-2`, `signer-8`, `signer-9`, `signer-10` |
| Aggregate fee bleed / negative unit-economics: per-success model OVERSTATES profitability; real dominant cost is priority+base fee burned across the ~90-99% of attempts that revert. With avg ~$1.58/arb and 50-70% tip lea… | **high** | Probabilistic cost-gate E[net] = p_land*(spread-fees-tip-prio-base) - (1-p_land)*(prio+base burned) - rent_churn - E[rug/honeypot loss], evaluated SYNCHRONOUSLY pre-sign (observ-4/observ-14). Instrument burn-rate (lamports/min on losers) AND revert-rate (>30%=infra bug) as first-class health metrics… | `observ-3`, `observ-4`, `observ-5`, `observ-14`, `signer-7`, `landing-3`, `landing-9` |
| Trust-boundary bypass via remaining_accounts: pool accounts arrive UNTRUSTED; a griefer can supply fake pool / wrong-owner token accounts to mislead the profit-assert into landing an unprofitable or exploitable tx (espec… | **critical** | On-chain program MUST verify (a) each swap-CPI target is an allowlisted DEX program id, (b) balance-read token-accounts are owned by the bot authority. Mirror the SAME allowlist in signer TxShapeValidator (dest=own-ATA) and txbuilder vetting so all three layers agree (single source: onchain/allowlis… | `onchain-3`, `onchain-8`, `signer-3`, `txbuilder-11`, `testing-6` |
| Tip-not-inside-atomic-tx leakage: if the Jito tip is placed in a separate tx or outside the reverting tx, a failed arb still pays the tip (sunk cost), and Frankendancer leaders do not honor bundle atomicity, so a re-broa… | **high** | Place tip transfer INSIDE the same atomic arb tx (fail => tip unpaid). Each arb tx carries its own profit/slippage guard independent of bundle atomicity. jitodontfront stamped; routing exclusive via Jito (no silent non-protected RPC fallback). Bundle-build hard-limit guard ensures tip ix doesn't pus… | `landing-4`, `txbuilder-13`, `txbuilder-5`, `landing-7` |
| Hard-limit ceiling collisions (128 account-locks binds BEFORE 256 loaded; 1232-byte cap NOT raised by ALT; signers cannot live in ALT): a multi-account route plus WSOL dance + tip + ComputeBudget ixs can silently exceed… | **high** | limits::validate gate (txbuilder-6) checks locks<128 (the real ceiling), serialized bytes<=1232, loaded<=256, CU<1.4M against the ACTUAL v0 message + ALT set + signers-in-static-keys before sign. CU/lock/byte budget asserts in testing-7. Constants centralized in arb-config::limits so all modules sha… | `scaffold-4`, `txbuilder-5`, `txbuilder-6`, `testing-7`, `landing-4` |
| Phase-estimate optimism / Fase-2 slip: §11 headline durations (Fase 0: 1-2wk, Fase 1: 3-5wk, Fase 2: 3-4wk) materially undershoot the summed task estimates and assume zero slip for one senior dev. §10 itself flags Fase 2… | **medium** | Plan to summed-estimate-with-parallelism: Fase 0 ~2.5-3.5wk, Fase 1 ~6-7wk (critical sub-chain sizing+onchain+gate ~30-34 dev-days), Fase 2 ~5-6wk. Add explicit contingency buffer. Gate phases on exit-criteria (esp. M1-GATE) not calendar. Parallelize independent module tracks aggressively since most… | `landing-2`, `landing-3`, `landing-4`, `landing-5`, `landing-6`, `signer-7`, `signer-8`, `observ-14` |

---

## 11. Definition of Done — Milestone 1 (mapping to owning tasks)

| `plan.md` §12 DoD item | Owning tasks |
|---|---|
| Native `TryArbitrage`: 2 swap CPI + terminal profit-assert; revert on `Err(Unprofitable)`, runtime rolls back all state | onchain-6, onchain-8, onchain-9 |
| Proven revert on unprofitable input (LiteSVM + Surfpool + mainnet small-size), no net token movement | testing-2, testing-8, onchain-9, landing-8, **add-2** (round-trip closure) |
| Off-chain predicted profit == on-chain realized profit — fuzz/property, per-venue, both directions | **M1-GATE** = onchain-10 + sizing-9 + testing-5 (collapsed, §9.3) |
| Trust boundary: swap-CPI only to allowlist; balance-read only from bot-owned accounts | onchain-3, onchain-8, signer-3, txbuilder-11, testing-6 |
| v0 tx + pre-warmed ALT; locks <128; tx ≤1232 B; CU <1.4M; `SetComputeUnitLimit` measured+10% | txbuilder-5, txbuilder-6, txbuilder-8, txbuilder-9, testing-7 |
| WSOL wrap/sync/close; Token-2022 filter (incl. mint-close-authority); profit-check from actual balance delta | txbuilder-3, txbuilder-4, onchain-5, testing-6 |
| Lands profitable ≥1× on mainnet via Jito 1-tx bundle, tip inside the atomic tx | landing-4, landing-8 |
| Landing loop: status polling + rebuild fresh blockhash on no-land | landing-6, landing-9, **dec-2** (cap lifecycle) |
| Key security: small-balance hot-key sidecar + KMS/multisig treasury; kill-switch + sweeper operational | signer-3, signer-4, signer-7, signer-8, signer-9, signer-10 |
| Observability: revert-rate, latency P50/P95, PnL dashboarded; deviation alerts | observ-2, observ-3, observ-5, observ-6, observ-13 |
| Unit economics documented: probabilistic E[net] > 0 on target niche (incl. loser-burn) | observ-3, observ-4, observ-14 |
| Concurrency safety on hot pools (one-inflight-per-writable-account) — *required for a safe first land* | **add-1** (BLOCKER) |
| Token-2022 fee correctness at runtime (live `getEpochFee`, no cross-epoch cache) | **add-4**, sizing-2 |
| Honeypot/rug pre-trade vetting for the fresh-launchpad niche | **add-3** |
| Runtime feature-gate detection (SIMD-0268/0339), no hardcoded CPI budget | **add-5** |
| Deploy upgradeable (Squads authority) + published verifiable build (`solana-verify`) | scaffold-12, onchain-13, signer-10, signer-11 |

**Coverage verdict [critique].** The plan is unusually thorough and, with the synthesis DAG, covers the LARGE majority of §12 Definition of Done for Milestone 1. The three overlapping rounding-mirror gate tasks (onchain-10, sizing-9, testing-5) are correctly reconciled into one M1-GATE go/no-go; trust-boundary (onchain-3/8 + signer-3 + txbuilder-11 + testing-6), hard limits (arb-config::limits as single source -> txbuilder-6 -> testing-7), ALT warm-up/janitor (txbuilder-8/9/10 + testing-7), WSOL CloseAccount (txbuilder-4), Token-2022 HARD-REJECT incl. MintCloseAuthority (onchain-5/txbuilder-3/testing-6), synchronous pre-sign caps (signer-4), kill-switch numeric thresholds (signer-7/observ-6), verifiable build + Squads (scaffold-12/onchain-13/signer-10), tip-inside-atomic + jitodontfront (landing-4), fresh-blockhash landing loop (landing-6), and burn-rate/revert-rate/E[net] economics (observ-3/4/6) all have clear owners with concrete acceptance criteria. RESIDUAL RISK is concentrated in a few load-bearing items that the spec explicitly flags but the DAG leaves UNOWNED or only mentions in risk-prose: (1) the one-inflight-per-writable-account concurrency mutex (§6 KRITIS) is a genuine BLOCKER gap — it is the dominant self-collision failure mode for the hot-pool niche and has no task; (2) route-specific SELL-simulation honeypot vetting (§9) and the live getEpochFee read (§9) are HIGH gaps directly tied to the niche (rug/honeypot, Token-2022 fee correctness) the plan targets; (3) the inventory round-trip-closure invariant (§6) is asserted nowhere; (4) runtime SIMD feature-gate detection (§6 line 260, explicitly 'do not hardcode') is unowned. None of these block the M1-GATE itself, but (1)-(3) block a SAFE first profitable mainnet land (DoD item 'Mendarat profitable... tanpa pergerakan token bersih' and the §10 fee-bleed/rug economics). Sequencing is sound: every mainnet-landing task (landing-2..9) sits in Fase 2 strictly after the Fase-1 M1-GATE, and txbuilder-5/landing-2 correctly depend on onchain-8/sizing-8, so 'mainnet landing before the rounding-mirror gate' is NOT present. Net: the plan satisfies §12 DoD on paper PROVIDED the concurrency-mutex, honeypot-SELL-sim, live-epoch-fee, round-trip-closure, and runtime-feature-gate tasks are ADDED before first mainnet capital; without them the system can pass the lab gate yet bleed fees / strand inventory on the first real hot-pool contention or Token-2022/rug edge case.

---

## 12. Consolidated open questions / decisions

Decisions the spec leaves open. The three highest-impact (Agave pin, cap lifecycle, `min_profit` definition) are promoted to `dec-1..3` in §9; the rest are tracked per module below.

- **`scaffold`** — Exact pinned versions: §8 gives Agave 2.x/4.x as a range and yellowstone as 'v13.x +solana.4.0.0' — needs a single concrete pin decision (I provisionally pinned… · Verified on-chain hash/program id for PumpSwap AMM and exact verified-on dates for all Wave-1 ids must be filled by an operator running the Solscan verification… · Truncated prop-AMM full ids (HumidiFi/Tessera/GoonFi) are intentionally left unverified — open whether to store the known partial prefixes at all or omit entire… · Whether arb-signer is a separate process/binary communicating over a local socket vs an in-process module — scaffold provisions a separate binary (safer isolati…
- **`onchain`** — min_profit semantics: base/priority/tip are SOL-lamport costs not visible as base-asset (WSOL/USDC) balance delta inside the program. · Whether to enforce venue-native min_out (other_amount_threshold / minimum_amount_out) strictly or set it permissive and rely solely on the terminal assert. · Orca Whirlpool tick-array selection for Fase-1 is client-resolved for a single pair; · Exact PumpSwap AMM instruction account layout/discriminators need on-chain verification (less documented than Raydium/Orca) before onchain-12 can be finalized.
- **`sizing`** — Whirlpool effective-reserve linearization for the CPMM closed form: Milestone-1 assumes the optimal trade stays within the current tick range so Ra/Rb can be de… · Exact Raydium CP-Swap and PumpSwap fee numerator/denominator conventions (trade_fee_rate scale, whether protocol fee is separate) must be pinned from the live p… · Default policy fraction within 90-95%: design defaults to 92/100. · min_profit unit and source: should it be a fixed lamport floor, or derived from current priority-fee+tip estimate per opportunity? The on-chain assert uses min_…
- **`detection`** — Whirlpool-vs-CPMM price normalization unit: detection emits best_spread_bps, but the exact normalized price basis (and whether spread should be computed off raw… · PumpSwap PoolState exact field offsets and fee model (constant-product fee bps location) are under-documented in the spec; · max_component_slot_skew for CPMM assembly: how stale can a vault be relative to PoolState before the pool is treated as Pending rather than Fresh? Needs an empi… · emit_threshold_bps / min_delta_bps defaults: where to set the dislocation gate so detection shortlists usefully without flooding the sizer — depends on the nich…
- **`txbuilder`** — TransferHook nuance: default is blanket HARD-REJECT (allow_null_transfer_hook=false), discarding genuinely-safe null-program-hook mints like PYUSD. · Static-ALT churn policy: when a new hot pool appears, do we always extend the single static long-lived table (accepting 1-slot warm-up before first use) or spin… · CU margin default: spec says measured+~10%; · Who owns getTipAccounts resolution and the 8-account load-balancer — landing/executor (assumed) — and exactly what struct does it hand the txbuilder tip module?
- **`landing`** — Exact profit_cap_frac value: spec says 'cap tip as fraction of profit' (cites tip wars eating ~50-70% on saturated pairs) but gives no number. · Tip-per-CU scaling curve within the 50th-75th band: how aggressively to lerp toward p75 as competition rises is unspecified; · no-land rebuild threshold: spec says ~2-3s; · Fan-out width N (how many backup regions beyond nearest) vs the 1 req/s/region limit and duplicate-bundle cost — needs a cost/land-rate experiment.
- **`signer`** — Exact day-1 numeric values for MAX_LAMPORT_OUT, hot_cap_lamports, and working_reserve_lamports depend on the chosen pre-funded inventory size (§10 'minutes-to-h… · Squads multisig signer set + approval threshold (e.g. · Whether sweeps to treasury should themselves require a second approval or run fully automated from the hot key — current design allows automated sweep-to-treasu… · On-call posture default (UnmannedAutoHaltOnly vs OnCall) — plan lists both as options;
- **`testing`** — Exact realized_out extraction contract from the on-chain DEX CPIs: confirmed approach is reading pre/post ATA balance deltas via SwapHarness, but if crates/sizi… · Which specific mainnet slot(s) to capture as canonical replay fixtures, and whether snapshots are stored in-repo (small) or fetched from object storage in CI (l… · Whether Surfpool/surfnet supports cloning ALL three Wave-1 programs' full account graphs (tick arrays, oracle accounts) reliably enough for deterministic assert… · CU baseline drift tolerance (proposed 10%) and whether to fail-hard or warn in CI until the program stabilizes.
- **`observ`** — Metric backend choice: Prometheus pull (assumed default to stay on the §10 $0-198/mo ladder) vs OTLP push to a managed collector. · p_land cold-start prior value and min_samples threshold: spec gives ~90-99% revert (so p_land ~1-10%) as an estimate, not a committed prior. · Exact numeric kill-switch thresholds beyond revert>30%: burn-rate lamports/min max, realized-loss SOL/hr max, and hot-key balance-deviation lamports are require… · Golden-replay tolerance: sizing predicted_out should be 0-bps (bit-exact mirror), but net-after-fees may legitimately drift with live epoch fees - need an agree…

---

## Appendix A — Provider / cost ladder (from `plan.md` §8)

| Phase | Data source | ~Cost/mo |
|---|---|---|
| 0–1 build + proof-of-mechanism (1 pair) | Jito ShredStream (free) + free WSS + JSON-RPC | ~$0–40 |
| 2 first profit (~20–50 pairs, targeted) | Chainstack Growth + Yellowstone gRPC add-on | ~$98–198 (flat) |
| 3 niche / firehose (~150–300 pairs) | Self-host yellowstone/richat, or Chainstack $449 add-on | ~$498–1,400 |
| competitive / liquid-pair | Triton dedicated / co-located + ShredStream | $2,900+ per region |

Landing is separate from the data stream: **Jito bundle** (primary) + **Helius Sender** (free, 0 credits) fallback. **Helius Business $499/mo is skipped** for M1 — Chainstack wins per-dollar and self-host wins at firehose scale.

## Appendix B — Reference repos (audited; from `plan.md` §13)

`buffalojoec/arb-program` (native-Rust skeleton) · `0xNineteen` rust-macros-arbitrage · `raydium-io/raydium-cpi-example` · `orca-so/whirlpool-cpi-sample` · `raydium-io/raydium-amm` (account counts & integer math) · `raydium-io/raydium-clmm` (tick math) · `rpcpool/yellowstone-grpc` · `jito-labs/shredstream-proxy` · `litesvm` · Surfpool.

> ⚠️ **Security:** never run the `Solana-Arbitrage-Bot` keyword-spam repo cluster (documented wallet-draining malware, SlowMist) with a funded key. Pin deps + lockfile + integrity hashes; sandbox everything; throwaway keypair.
