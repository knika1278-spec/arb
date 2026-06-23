# TODO — Solana Atomic Arbitrage (Milestone 1)

> Execution checklist derived from [`implementation-plan.md`](./implementation-plan.md) §6 (the 107-task DAG)
> and §9 (the critique addendum), with status grounded against the current repo tree.
> `plan.md` = *what & why* · `implementation-plan.md` = *how/order/done-when* · this file = *track it*.
>
> **The single hard gate is `M1-GATE`** (onchain-10 + sizing-9 + testing-5, collapsed): off-chain
> predicted output must equal on-chain realized output, bit-exact, per-venue, both directions,
> incl. the Token-2022 fee path. **No mainnet send before it is green.**
> Full acceptance criteria ("Done when") live in `implementation-plan.md` §5; this file is the index.

## Legend

- `- [x]` done & green
- `- [ ] 🟡` partial / skeleton / placeholder (started, not complete)
- `- [ ] 🔒` blocked on platform-tools (`build-sbf`/BPF, LiteSVM-loads-program, Surfpool, real gRPC, v0-tx assembly — see [[arbit-build-environment]]) or an upstream operator action
- `- [ ]` not started
- `★` = on the critical path to first profitable mainnet land
- `(module · est · deps)` = owning module · solo-senior-dev day estimate · upstream task IDs

---

## Status snapshot (2026-06-22)

Workspace is **green**: `cargo build/test/clippy -D warnings/fmt` pass (**234 tests**, host + WSL), `config-check` validates.

> **2026-06-22 BREAKTHROUGH — `build-sbf` + M1-GATE unblocked (WSL2 Ubuntu 24.04).** The on-chain `.so` now builds (`cargo build-sbf --tools-version v1.54`, agave 2.3.13) and the **M1-GATE rounding-mirror differential is GREEN for the constant-product path** in LiteSVM (`tests/litesvm-tests/tests/m1_gate.rs` + `tests/swap-harness`): on-chain realized round-trip == off-chain `arb_math::RoundTrip::realized_out`, bit-exact (3 sizes), plus revert-on-unprofitable + reject-non-allowlisted-dex. CPI discriminators filled (Anchor sha256). **dec-1 closed = Agave 2.3.x.** Residual for FULL M1-GATE: Surfpool real-venues (onchain-11), Token-2022 fee path, BtoA/Orca sqrt-price.

- **Done:** workspace + 8 members; `arb-config` (no_std allowlist+limits, std providers/secrets/loader); `arb-types` (ArbError 6000-base, DexKind, SwapDir, **`CostTerms`/`min_profit` = dec-3**); **`arb-math` = the M1-GATE math core** (bit-exact cpmm quote/required-in, RoundTrip, opportunity predicate, Token-2022 fee fwd/inverse, optimal-delta search, 92.5% policy); `onchain/arb-program` skeleton (unpack/pack, trust boundary, zero-copy balance read, Token-2022 vetting, processor); `detection` module (idempotent cache, pair graph, decode, reconnect, gRPC seam); `sizing` wrapper.
- **Done (host-logic, this session):**
  - **`txbuilder`** (prior session): build_arb_tx→BuiltTxPlan, layout, compute, wsol dance, limits gate, token2022 mirror, vet, alt manager, preflight seam.
  - **`metrics`/observ** (observ-1,2,3,4,5,6,9): lock-free registry+drop-cause histogram, latency P50/P95 + ConfirmationRank, slippage bps/route, PnL windowed burn-rate/loss/revert-rate, deterministic-i128 CostModel + p_land EWMA, HealthEvaluator→KillSwitchSignal.
  - **`signer`** (signer-2,3,4,5,6, thin 7 + add-2 + dec-2): MemorySigner (openssl-free), TxShapeValidator over the ix list, synchronous PreSignCaps, atomic sign path, killswitch handle+supervisor, alert seam.
  - **`executor`/landing** (landing-3,6,8,10 + add-1 BLOCKER): types, TipOracle::size_tip, WritableAccountRegistry (one-inflight-per-pool), landing-loop state machine (BlockhashSource/LandingTransport seams), Executor::land facade.
  - **`analytics`** (observ-10,11,12): GoldenSample corpus loader, replay gate CLI (`gate`/`replay`/`backtest`, CI-blocking exit, reuses live arb-math mirror + CostModel), backtest. Sample corpus + README; gate runs end-to-end at 0 bps.
- **Blocked on `build-sbf`/runtime/heavy-net-crates:** venue CPI discriminators (adapters are `[0;8]` placeholders), the M1-GATE differential, LiteSVM program-load tests, Surfpool revert proof, real Yellowstone gRPC client, real JitoClient/HeliusSender/SWQoS/RpcClient (`LandingTransport`/`BlockhashSource` impls), v0-tx assembly (needs solana-message), getEpochFee (add-4), SELL-sim honeypot gate (add-3), deploy posture (onchain-13/signer-10).

**Immediate next (host-doable now, no `build-sbf`):** observ-7 Prometheus exporter / observ-8 alert router (need hyper/reqwest-rustls), observ-13 Grafana dashboard JSON, sweeper logic (signer-8 minus RPC), add-5 feature-gate seam, add-6 Whirlpool tick-array PDA resolver (derivation host-doable). Then unblock the gate when platform-tools land.

---

## On-chain reference examples (verified 2026-06-22)

Real mainnet arb txs cross-checked on Solscan during the @uyar121 article audit. Kept here as concrete
shape references + **golden-sample corpus candidates** (feed `observ-10` / `analytics/corpus`). All are
v0-tx + ALT + ComputeBudget, single atomic tx via a third-party arb program — i.e. exactly the M1 shape.

| Pattern | Venues | Signature (Solscan) | M1 scope | Note |
|---|---|---|---|---|
| DEX-to-DEX | PumpSwap AMM → Raydium **CLMM** | `5zwwFzqsDFRf6FnGz5rrAbWuFPExqb7tYmSk1uTvWF8x7Cu272XFPoU8nCfnAXbCYZTHV74PDgTqm9hSQrf8uX8j` | ⚠️ leg-B CLMM out (was) | elun; +3.35 SOL net; tip ix `jesterKqz…` |
| Triangle | Meteora DAMM v2 ×2 → DLMM | `J8TY8VkjZpAAm78GwbnEE1xkBwGdheQ4C1VsZA7Cwcv1AyDw3PxTQJ3eWh9YZmZyLQnLD3fuHBsihXbX4sTATi8` | Fase 2.5 | ANB; 0.227→696,194 USDC; Jito tip 2.30 SOL inside tx (ix #5) |
| Internal-DEX | Meteora DAMM v2 → DLMM | `3pb5512ttABHr8mKCM8MTfqTHqbQYxuFMQu8vrof6j24fdRz3LSp4LiLoNhbjtNec43UjFivQGTjjDEKaJ5AYhtx` | Fase 2.5 | ANB; via `sattC…` arb bot; 1.0 SOL priority |
| Internal-DEX | Meteora DAMM v2 → DLMM | `3GRCRJmVhSKM2M1wcZ1vQVvNwUT2j5WWJhufVzvY2EMa8rE3mu5PjDUCpHVpXkth26wqhTuWFHkixwdy7zGtWC1h` | Fase 2.5 | ANB; bribe 141.3 SOL ($10.5k) — bribe-war evidence |

> Validates: atomic single-tx + ALT + tip-inside-tx (invariant #10) + tip-per-CU bribe auction (`txbuilder-13`).
> The three ANB jackpots live in venues M1 originally deferred (Meteora) — now promoted under Fase 2.5 below.

---

## Fase 0 — Foundation (20 tasks)

*Exit: workspace builds w/ committed lockfile; arb-config/arb-types consumable; all Wave-1 program IDs verified on Solscan; onchain skeleton builds + LiteSVM smoke green; supply-chain gate green; hot-key chmod 600.*

- [x] ★ `scaffold-1` Init git repo, monorepo skeleton, .gitignore secrets guard *(scaffold · 0.5d · —)*
- [x] ★ `scaffold-2` Pin toolchain: rust-toolchain.toml + versions.toml + bootstrap *(scaffold · 1d · scaffold-1)*
- [x] `scaffold-6` Author infra/config TOMLs: program_ids, providers, limits *(scaffold · 0.5d · scaffold-1)*
- [x] ★ `scaffold-3` Cargo workspace + centralized pinned deps + committed lockfile *(scaffold · 1.5d · scaffold-2)*
- [x] ★ `scaffold-4` arb-config no_std core: program_ids + limits constants *(scaffold · 1d · scaffold-3)*
- [x] `scaffold-5` arb-config std: providers/landing, secrets loader, loader+validate *(scaffold · 1.5d · scaffold-4,6)*
- [x] `scaffold-9` Supply-chain integrity: deny.toml, integrity-hashes, cargo-audit/deny *(scaffold · 1d · scaffold-3)*
- [ ] 🟡 `scaffold-7` Config-consistency tooling: verify-config.sh + Solscan cross-check *(scaffold · 0.5d · scaffold-5,6)* — script done; **operator must run on-chain Solscan verification of all Wave-1 IDs (esp. PumpSwap AMM)**
- [x] `scaffold-8` Key/program-keypair gen script + secrets contract enforcement *(scaffold · 0.5d · scaffold-5)*
- [ ] 🟡 `scaffold-11` LiteSVM + Surfpool test substrate wiring + smoke test *(scaffold · 1d · scaffold-4)* — LiteSVM 0.7 fully wired + loads the real build-sbf .so (program_exec.rs green); 🔒 Surfpool pool-clone pending
- [x] `scaffold-10` CI pipeline: build/lint/test/lockfile/audit/config gates *(scaffold · 1d · scaffold-7,9,11)*
- [x] ★ `onchain-1` Crate scaffold + entrypoint + verifiable-build setup *(onchain · 2d · scaffold-4)*
- [x] ★ `sizing-1` Wide integer-math primitives: U256, mul_div, rounding *(sizing · 1.5d · scaffold-4)*
- [ ] 🟡 `detection-1` Detection config + venue program-id verification *(detection · 1.5d · scaffold-5,6)* — config done; on-chain ID verification pending operator
- [x] `txbuilder-1` Module scaffold, config, hard-limit constants *(txbuilder · 1.5d · scaffold-4)*
- [x] `txbuilder-2` ComputeBudget instruction builder + measured-CU sizing *(txbuilder · 1d · txbuilder-1)*
- [x] `signer-1` Key security baseline + supply-chain hygiene gate *(signer · 2d · scaffold-1,9)*
- [x] `signer-2` SolanaSigner trait + MemorySigner hot-key backend *(signer · 2d · signer-1, scaffold-5)* — keychain.rs: trait + MemorySigner (solana-keypair/signer/signature, openssl-free) + only-Memory-hot-path assert
- [ ] 🟡 `landing-1` Jito account, UUID and Sender baseline (Fase 0 setup seam) *(landing · 2d · scaffold-1)* — **code seam DONE**: `JitoConfig::resolve_auth_uuid`+`x-jito-auth` header (arb-config), `validate` rejects blank auth_uuid_env/landing endpoints, executor `setup.rs` (`TipAccountSource` 8-acct **runtime** seam + `TipAccountSet` validation `from_resolved` exactly-8/distinct/non-default, `JitoAuth`, `SenderEndpoint`+`EndpointProbe` reachability seam), wired in `main.rs` (resolves UUID from env, never logged). 🔒 operator must provision Jito allowlisted UUID → `JITO_AUTH_UUID` + register Helius Sender; real `getTipAccounts` RPC resolution + reachability HTTP land in landing-2/-7
- [x] `testing-1` Fase 0: toolchain + LiteSVM bootstrap + skeleton build *(testing · 2d · onchain-1, scaffold-11)*

---

## Fase 1 — On-chain TryArbitrage + bit-exact sizing + M1-GATE (51 tasks)

*Exit: TryArbitrage reverts `Unprofitable` on no-arb (LiteSVM) & succeeds with exact delta; **M1-GATE GREEN** (predicted == realized, both dirs, both DEX, incl Token-2022); trust boundary enforced; locks<128 / tx≤1232B / CU<1.4M; ALT pre-warmed; WSOL dance clean; detection cache idempotent; revert proven on Surfpool fork.*

### onchain
- [x] ★ `onchain-2` Error enum + instruction-data layout + Dex/LegDescriptor *(onchain · 2d · onchain-1)*
- [x] `onchain-3` Pinned allowlist + trust-boundary verification *(onchain · 2d · onchain-1,2)*
- [x] `onchain-4` Zero-copy balance read (state.rs) *(onchain · 1d · onchain-1)*
- [x] `onchain-5` Token-2022 extension filter (token2022.rs) *(onchain · 2d · onchain-1,2)*
- [ ] 🟡 ★ `onchain-6` Raydium CPMM swap adapter *(onchain · 3d · onchain-3,4,5)* — adapter + Anchor `swap_base_input` discriminator filled; CP math proven via LiteSVM M1-GATE; real-venue rounding pending Surfpool 🔒
- [ ] 🟡 `onchain-7` Orca Whirlpool swap_v2 adapter *(onchain · 3d · onchain-3,4,5)* — `swap_v2` discriminator filled; sqrt-price mirror still CP-approx, pending Surfpool 🔒
- [ ] 🟡 ★ `onchain-8` Processor: snapshot→CPI A→delta→CPI B→terminal assert *(onchain · 3d · onchain-6,7)* — skeleton done; awaits real adapters
- [ ] 🟡 ★ `onchain-9` LiteSVM unit tests: revert, success, trust-boundary, CU *(onchain · 3d · onchain-8)* — success + revert-unprofitable + non-allowlisted-dex GREEN (m1_gate.rs); CU-budget assert + Token-2022 negative still TODO
- [ ] 🟡 ★ `onchain-10` Rounding-mirror fuzz/property gate (per-venue, both dirs) — **M1-GATE** *(onchain · 4d · onchain-9, sizing-8)* — CP path GREEN via LiteSVM (3 sizes, AtoB); both-dirs + per-venue fuzz + Surfpool real-venue residual
- [ ] 🔒 `onchain-11` Surfpool mainnet-fork integration test (revert on real programs) *(onchain · 3d · onchain-10)*

### sizing
- [x] `sizing-2` Token-2022 transfer-fee forward/inverse math *(sizing · 1d · sizing-1)*
- [x] ★ `sizing-3` Quoter trait + QuoteIn/Out/SwapDir/QuoteError + venue registry *(sizing · 1d · sizing-1,2)* — `arb-math/venue.rs`: object-safe `Quoter` (`quote_exact_in`/`quote_required_in`/`marginal_price_x64`+`approximate`); `QuoteIn`, `QuoteOut{gross_in,net_in,gross_out,net_out}` (net distinct from gross via Token-2022 `TransferFeeConfig` per side — profit-check on balance delta), `QuoteError`. `CpmmVenue::with_transfer_fees` + `dyn_round_trip_net_out(&dyn Quoter,&dyn Quoter)` proves object-safety. **M1-GATE core untouched** (concrete `RoundTrip`/`CpmmReserves::quote_out` not routed through the trait). Venue registry `sizing::venue_program_id(DexKind)→Pubkey` byte-equal the arb-config allowlist (shared test asserts `is_allowlisted_swap_program`). arb-math 28 + arb-bot 183 tests green.
- [x] ★ `sizing-4` Raydium CP-Swap Quoter (bit-exact) *(sizing · 1.5d · sizing-3)* — impl+host-tested; bit-exactness proven only by M1-GATE
- [x] `sizing-5` Orca Whirlpool Quoter (bit-exact, in-range) *(sizing · 2.5d · sizing-3)* — `arb-math/whirlpool.rs` (673 lines): faithful Orca `compute_swap` port — `WhirlpoolPool::quote_exact_in(dir, amount_in, sqrt_price_limit)` Q64.64 sqrt-price both dirs, Floor-output/Ceil-input per direction, fee-on-input floor, `CrossesTick` at the boundary tick (resolved off-chain by add-6); 256-bit intermediates via existing `U256` (no_std, `arithmetic_side_effects=deny`). **Recovered from stranded commit 9031997** (was in `worktree-arbit-fase-unlock`, absent from main) + re-verified: 16 host tests (hand-fixtures delta_a=L/2 / delta_b=L, both-dir hand values, fee/rounding/CrossesTick/round-trip-lossless) GREEN — arb-math 46 tests. On-chain `swap_v2` CPI differential (onchain-7, 🔒 build-sbf) remains.
- [x] `sizing-6` PumpSwap AMM Quoter (bit-exact) *(sizing · 1d · sizing-3)* — `CpmmVenue::pumpswap(base,quote,lp_bps,protocol_bps,coin_creator_bps)`: sums the 3 fee components **once** into one numerator over `PUMPSWAP_FEE_DENOMINATOR`=1e4, then the bit-exact CP path (fee-on-input pre-swap, x·y=k floor) via the sizing-3 `Quoter`. Bit-exact test both directions vs the single-application cpmm reference + concrete fixture (30bps, 10k in → 19_743 out) + double-apply guard + degenerate-fee reject. arb-math 30 tests green.
- [x] ★ `sizing-7` RoundTrip composite + CpmmReserves extraction *(sizing · 1d · sizing-4,5,6)*
- [x] ★ `sizing-8` Closed-form delta* + opportunity predicate + policy (90-95%) *(sizing · 2d · sizing-7)*
- [ ] 🟡 `sizing-9` GATE: per-venue both-direction differential/property test *(sizing · 2.5d · sizing-8, onchain-9)* — part of M1-GATE; CP/AtoB differential GREEN, both-dir + fuzz residual

### detection
- [x] `detection-2` Core model + SessionStamp dedupe types *(detection · 1d · detection-1)*
- [x] `detection-3` Per-venue decoders (CPMM vaults+PoolState, Whirlpool, PumpSwap) *(detection · 4d · detection-2)* — **offsets VERIFIED + decoders implemented (2026-06-23)**. `decode.rs`: full `RaydiumCpmmPool`/`AmmConfig`-fee, `Whirlpool`, `PumpSwapPool`/`GlobalConfig`-fee decoders w/ named offset consts + account discriminators (`sha256("account:<Name>")`) + fee denominators (CPMM 1e6, Whirlpool 1e6, PumpSwap 1e4=lp20+proto5+cc5). Every offset triple-verified (struct byte-arithmetic + adversarial re-derivation + live `getAccountInfo`) and **locked by 5 real-mainnet-byte fixtures** (Chainstack RPC); Whirlpool offsets **independently re-confirmed 2026-06-23 against the official Orca IDL v0.3.0** (`whirlpool_idl.json`, computed from struct field sizes incl. WhirlpoolRewardInfo=128B → LEN 653: all 9 offsets match). Fail-closed on bad disc/short buffer. Wiring to a live stream still rides detection-5 (real gRPC, 🔒) / detection-9 (PumpSwap subscription).
- [x] `detection-4` Idempotent pool-state cache + CPMM multi-component assembly *(detection · 3d · detection-3)*
- [ ] 🟡 `detection-5` Yellowstone gRPC ingest client *(detection · 2.5d · detection-2)* — seam (AccountUpdateSource + MockSource) done; 🔒 real client deferred (heavy crate)
- [x] `detection-6` Token-pair graph + incremental edge recompute *(detection · 2d · detection-4)*
- [x] `detection-7` Reconnect/replay supervisor + run-loop wiring *(detection · 3d · detection-4,5,6)*

### txbuilder + ALT
- [x] `txbuilder-3` Token-2022 HARD-REJECT extension filter (mirrors onchain) *(txbuilder · 2.5d · txbuilder-1)*
- [x] `txbuilder-4` WSOL dance helper (wrap→sync→close) *(txbuilder · 1.5d · txbuilder-1)*
- [x] `txbuilder-8` ALT manager: create/extend/resolve append-only + ~30 chunking *(txbuilder · 2.5d · txbuilder-1)*
- [x] `txbuilder-9` ALT warm-up gate + static-table set (never extend-then-use same slot) *(txbuilder · 2d · txbuilder-8)*
- [x] `txbuilder-10` ALT janitor (async close after ~512 slots) *(txbuilder · 1.5d · txbuilder-8)*
- [x] `txbuilder-5` Canonical instruction layout + core message assembler *(txbuilder · 2.5d · txbuilder-2,4,9, onchain-8, sizing-8)* — layout/framing + BuiltTxPlan + `message::compile_v0_message` (v0 `VersionedMessage` via `MessageV0::try_compile` with pre-warmed ALTs + blockhash; UNSIGNED — signing/serialize stays the signer seam). Host-green (157 arb-bot tests)
- [x] `txbuilder-6` Hard-limit validation gate (locks<128, bytes≤1232, CU<1.4M) *(txbuilder · 2d · txbuilder-5)*
- [ ] 🟡 `txbuilder-7` Preflight simulateTransaction wrapper + profit-check *(txbuilder · 2.5d · txbuilder-5,2)* — profit_from_balances + SimulateRpc seam done; real RPC client deferred
- [x] `txbuilder-11` Pre-build route vetting (filter + DEX allowlist mirror + frozen-ATA) *(txbuilder · 1.5d · txbuilder-3,4)*
- [ ] 🔒 `txbuilder-12` End-to-end build harness on mainnet-fork (LiteSVM/Surfpool) *(txbuilder · 2.5d · txbuilder-6,7,9)*

### signer
- [x] `signer-3` TxShapeValidator (allowlist, dest=own-ATA, max-lamport-out, tip) *(signer · 3d · signer-2, onchain-8)* — validates the ix list (top-level program allowlist + System-transfer dest classification + signer-in-ALT + add-2 base-ATA closure)
- [x] `signer-4` Synchronous PreSignCaps (count + cumulative lamport-out) *(signer · 2d · signer-2)* — incl. dec-2 release-before-reserve = 1 slot/opp
- [x] `signer-5` SignerSidecar canonical sign path (flag→shape→caps→sign, atomic) *(signer · 2d · signer-3,4)* — key never touched on a failed gate (CountingSigner test)

### testing
- [x] `testing-2` Pool + mint builders for LiteSVM *(testing · 4d · testing-1, sizing-7)* — token-account + pool builders in m1_gate.rs (direct-balance harness model)
- [x] `testing-3` SwapHarness test program + single-leg client *(testing · 3d · testing-2)* — `tests/swap-harness` CP program (build-sbf), driven by m1_gate.rs
- [ ] 🔒 `testing-4` LiteSVM unit tests: revert + exact-delta + boundary *(testing · 3d · testing-2,3, sizing-4, onchain-8)*
- [ ] 🟡 ★ `testing-5` **M1-GATE**: differential/property rounding-mirror test *(testing · 5d · testing-2,3, sizing-8, onchain-10)* — CP differential GREEN (m1_gate.rs); fuzz + both-dirs + Token-2022 + Surfpool residual
- [ ] 🔒 `testing-6` Trust-boundary + Token-2022 filter negative tests *(testing · 3d · testing-2,4, onchain-3,5)*
- [ ] 🔒 `testing-7` CU / account-lock / tx-byte budget + ALT pre-warm asserts *(testing · 2d · testing-4, txbuilder-9)*
- [ ] 🔒 `testing-8` Surfpool mainnet-fork integration vs real Raydium/Orca *(testing · 5d · testing-4,6, txbuilder-12)*
- [ ] 🔒 `testing-9` Deterministic historical replay (Yellowstone / Old Faithful) *(testing · 3d · testing-5,8)*

### observability + build pipeline
- [x] `observ-1` Metric registry + canonical keys (lock-free hot path) *(observ · 2d · scaffold-3)* — AtomicU64 counters + drop-cause histogram + 8-thread stress test
- [x] `observ-4` Probabilistic cost model + synchronous cost-gate *(observ · 2.5d · scaffold-3, sizing-8)* — deterministic i128 e_net/gate; observ-5 PLandEstimator EWMA also done
- [x] `observ-10` Golden-replay corpus format + loader *(observ · 1.5d · scaffold-3)* — analytics crate; observ-11 gate + observ-12 backtest also done (reuse arb-math mirror + CostModel)
- [x] `observ-2` Latency spans (P50/P95) + confirmation-rank capture *(observ · 1.5d · observ-1)* — lock-free exp-bucket histogram, <1% on uniform; SpanGuard records on drop
- [x] `observ-3` PnL ledger + burn-rate accumulator *(observ · 2d · observ-1)* — windowed burn-rate/loss/revert-rate (logical clock); reverted⇒tip_paid==0 invariant. observ-6 HealthEvaluator + observ-9 slippage also done.
- [ ] 🟡 `scaffold-12` Verifiable/reproducible build pipeline (solana-verify) + Squads deploy *(scaffold · 1d · scaffold-10)* — verifiable-build.yml exists; deploy path partial

---

## Fase 2 — First profitable mainnet land via Jito (31 tasks)

*Exit: landed profitable ≥1× on mainnet small-size via Jito 1-tx bundle, **tip inside the atomic tx**; tip accounts via getTipAccounts at runtime + jitodontfront + routing-exclusive; landing loop polls status + rebuilds fresh blockhash; Helius Sender/SWQoS fallback; PumpSwap integrated; signer kill-switch + sweeper live; full observability + cost-gate wired; deployed upgradeable w/ Squads + verifiable build.*

### onchain / detection / txbuilder
- [ ] 🟡 `onchain-12` PumpSwap AMM adapter (Fase 2 venue) *(onchain · 3d · onchain-8,10)* — placeholder; needs verified account layout/discriminators 🔒
- [ ] 🔒 `onchain-13` Deploy upgradeable + publish verifiable build (Squads authority) *(onchain · 2d · onchain-11, scaffold-12)*
- [x] `detection-8` Detection metrics + latency instrumentation *(detection · 1.5d · detection-7, observ-2)* — `detection/metrics.rs` `DetectionMetrics` (lock-free `AtomicU64`): `updates_total`, `cache_rejected_total{StaleSlot,Duplicate}`, `reconnects_total`, `gap_reconciles_total`, `decode_errors_total{venue}` (per-`DexKind`), hot/stale-pool gauges, **ingest→edge latency histogram P50/P95** (reuses observ-2 `Histogram`, now `pub`). `cache::apply_classified`→`ApplyOutcome` attributes the dedupe reason; `DetectionPipeline::on_pool_update_metered` wires it live. Field-struct ⇒ no duplicate-registration panic. Integration test increments per-venue decode-error on a real bad-discriminator buffer.
- [ ] `detection-9` Fase-2 targeted subscription sizing (20-50 pairs) + PumpSwap integration *(detection · 2.5d · detection-3,7)*
- [x] `txbuilder-13` Jito tip instruction (Fase 2 seam) + tip capping *(txbuilder · 1.5d · txbuilder-5)* — `jito_tip_ix` (System transfer inside the atomic tx) + `build_capped_tip_ix` (rejects tip > cap_frac·profit)
- [ ] `txbuilder-14` PumpSwap AMM venue support in builder/vet *(txbuilder · 1.5d · txbuilder-11)*

### signer (key mgmt + kill-switch + deploy)
- [x] `signer-6` KillSwitch flag + handle (manual halt < seconds, no auto re-arm) *(signer · 2d · signer-5)* — Arc<AtomicBool> + halt/ack/rearm + append-only JSON TripRecord (persist+reload test)
- [ ] 🟡 `signer-7` KillSwitchSupervisor + numeric thresholds + alert routing *(signer · 3d · signer-6, observ-6)* — `apply_health_signal` maps observ `KillSwitchSignal`→`HaltReason` + halts + alerts (thresholds live in observ `HealthEvaluator`, no dup); AlertSink trait + LogSink — Telegram/PagerDuty sinks deferred (reqwest)
- [ ] 🟡 `signer-8` Blast-radius sweeper (cron + threshold) to cold treasury *(signer · 3d · signer-7)* — `decide_sweep` (surplus = bal − working_reserve − rent; never below floor; treasury-only dest; cron vs threshold) done; async cron task + RPC submit + sweep-sign-during-halt seamed
- [ ] 🟡 `signer-9` Hot-key rotation + working-capital funding ops *(signer · 2d · signer-8)* — rotate_hot_key.sh exists; runtime ops pending
- [ ] 🟡 `signer-10` Deploy posture: Squads upgrade authority + solana-verify reproducible *(signer · 3d · signer-1, scaffold-12)* — verifiable-build + deploy script scaffolded
- [ ] 🟡 `signer-11` Kill-switch recovery runbook + manual-halt drill + on-call posture *(signer · 2d · signer-7,9)* — runbook exists; drill not run
- [ ] 🔒 `signer-12` End-to-end signer integration test on mainnet-fork (Surfpool) *(signer · 3d · signer-8,10)*

### landing / executor (critical path to first land)
- [ ] ★ `landing-2` JitoClient JSON-RPC + regional fan-out + rate limiter *(landing · 4d · landing-1, txbuilder-5)*
- [ ] 🟡 ★ `landing-3` TipOracle: tip_floor REST + tip_stream WS, sizing + load-balance *(landing · 3d · landing-2, txbuilder-13)* — size_tip (band lerp + profit cap + stale fallback) + 8-account round-robin done; REST/WS feed deferred (network)
- [ ] ★ `landing-4` Bundle build: tip-inside-atomic-tx + jitodontfront + hard-limit guard *(landing · 4d · landing-3, txbuilder-6)*
- [ ] ★ `landing-5` Pre-tip simulation gate (simulateTransaction / simulateBundle) *(landing · 3d · landing-4, txbuilder-7)*
- [x] ★ `landing-6` Strict landing loop with fresh-blockhash rebuild *(landing · 4d · landing-5)* — state machine + distinct-blockhash assert + BlockhashSource/LandingTransport seams (landing-10 durable-nonce seam folded in)
- [ ] 🟡 ★ `landing-7` Helius Sender fallback + SWQoS non-bundle + routing-exclusivity guard *(landing · 3d · landing-6)* — Route enum + is_jito_protected + routing-exclusivity check in facade; real Sender/SWQoS clients deferred (network)
- [x] ★ `landing-8` Executor facade, route selection, signer handshake *(landing · 3d · landing-7, signer-5)* — Executor::land: killswitch→cost-gate→add-1 dedupe→tip→loop→metrics; SignerHandle seam
- [x] ★ `landing-9` Executor metrics: revert-rate, burn-rate, latency, drop-cause *(landing · 2d · landing-8, observ-1)* — facade records into observ MetricsRegistry (drop-cause→RevertCause map)

### testing + observability
- [ ] 🔒 `testing-10` Fase-2 forward hook: PumpSwap differential + Surfpool clone *(testing · 3d · testing-5,8, onchain-12, sizing-6)*
- [x] `observ-5` p_land EWMA estimator (per route + tip bucket) *(observ · 2d · observ-4,3)*
- [x] `observ-6` Health evaluator + numeric kill-switch thresholds *(observ · 2d · observ-2,3)*
- [x] `observ-7` Prometheus exporter + /healthz (off hot path) *(observ · 1.5d · observ-1,6)* — text exposition + healthz JSON + dependency-free `std::net` server (route() unit-tested)
- [ ] 🟡 `observ-8` Deviation-alert router (Telegram/PagerDuty) + runbook links *(observ · 1.5d · observ-6)* — `AlertRouter` per-reason dedup + runbook URL over `AlertSink`; Telegram/PagerDuty sinks deferred (reqwest)
- [x] `observ-9` Realized-slippage-per-route instrumentation *(observ · 1d · observ-1)* — signed-bps per (venue_pair,direction); 0 bps under the bit-exact mirror
- [x] `observ-11` Golden-replay regression gate (predicted vs realized) — CI-blocking *(observ · 2.5d · observ-10,4)* — `analytics gate` reuses arb-math mirror + CostModel, nonzero exit on drift; verified end-to-end at 0 bps
- [x] `observ-12` Aggregate backtest + unit-economics confirmation report *(observ · 1.5d · observ-10,4,5)* — `analytics backtest`: predicted vs realized E[net], revert-rate, burn, model bias
- [x] `observ-13` Grafana dashboard + deviation alert rules *(observ · 1d · observ-7,9)* — `analytics/dashboards/grafana-arbit-health.json`: revert-rate(30% line+alert), burn-rate, P50/P95, PnL, confirmation, slippage panels (valid JSON, exporter metric names)
- [x] `observ-14` Wire cost-gate into signer pre-sign + health into kill-switch (integration seam) *(observ · 1.5d · observ-4,6, signer-5,7)* — `signer/presign.rs`: `PreSignGate`/`evaluate_pre_sign` runs the signer-owned flag (`KillSwitchHandle`) THEN the metrics-owned `CostModel::gate` synchronously pre-sign; health→flag route reuses signer-7 `apply_health_signal`. Contract doc'd (metrics owns gate+signal LOGIC, signer owns flag+cap STATE, no duplication). Integration tests: EV-negative `CostInputs` rejected before a fake signer is touched; simulated >30% revert-rate spike → `HealthEvaluator`→`Trip`→supervisor flips `signing-enabled=false`→pre-sign returns `Halted`; flag-checked-before-gate ordering.

---

## Fase 3 — Forward seams only for M1 (8 tasks)

*Compile-gated seams so the Fase-3 venue/triangular/flash-loan build can be added without redesign.*

> **Added 2026-06-22 (post on-chain audit):** `onchain-15`/`onchain-16` capture the two strategies seen
> on-chain that had no seam in the DAG — **Buy & Unstake (LST)** and **Buy & Remove Liquidity**. Design-only
> for M1 (compile-gated, not implemented). Each needs a sizing + detection counterpart when promoted.

- [ ] `onchain-14` FORWARD SEAM: PDA-vault / invoke_signed abstraction hook *(onchain · 1d · onchain-8)*
- [ ] `onchain-15` FORWARD SEAM: **Buy & Unstake (LST)** — buy LST via aggregator/AMM → unstake-leg CPI back to base SOL (Marinade/Jito/Sanctum stake-pool); needs `sizing-11` unstake-rate math. Design-only; gated. *(onchain · 1.5d · onchain-8)* — pattern from audit (0xRappz STACSOL); NOT in M1
- [ ] `onchain-16` FORWARD SEAM: **Buy & Remove Liquidity** — buy LP-token from market → remove-liquidity CPI to claim underlying (base + WSOL); needs `sizing-11` LP-redemption math. Design-only; gated. *(onchain · 1.5d · onchain-8)* — pattern from audit (LP-token re-paired with WSOL); NOT in M1
- [ ] `sizing-11` FASE-3 SEAM: unstake-rate (LST→SOL) + LP-redemption value math for `onchain-15`/`onchain-16` (gated, design-only) *(sizing · 1.5d · sizing-8)*
- [ ] `sizing-10` FASE-3 SEAM: golden-section search + Bellman-Ford cycle (gated) *(sizing · 2d · sizing-8)*
- [ ] `detection-10` FASE-3 forward-hook: owner-firehose discovery seam *(detection · 1d · detection-4)*
- [ ] `signer-13` FORWARD HOOK: KMS/Fireblocks treasury backend seam *(signer · 1d · signer-2)*
- [x] `landing-10` Durable-nonce forward seam (design-only for M1) *(landing · 1d · landing-6)* — `BlockhashSource` trait with `is_durable_nonce()`; loop reads blockhash via the seam, durable-nonce variant disabled for M1

---

## Fase 2.5 — Scope expansion: Meteora + Raydium CLMM + triangle (ACTIVE · added 2026-06-22)

> ⚠️ **This widens M1 beyond the original "atomic 2-swap" Definition of Success.** Added per explicit user
> request after the on-chain audit (ANB triangle/Meteora + elun PumpSwap→Raydium-CLMM txs above), which
> showed the largest opportunities live in venues/patterns M1 deferred. Two consequences to keep honest:
> 1. **M1-GATE still applies per-venue, both-directions.** None of these venues is mainnet-eligible until
>    its differential is GREEN — `M1-GATE-EXT` below is the gate, not a formality.
> 2. **Triangle changes the on-chain program from 2-leg → N-leg** and the math from closed-form → cycle-based
>    (promotes the `sizing-10`/`detection-10` seams to active). Treat it as a distinct sub-milestone, not a drop-in.
>
> Conflicts with the standing "follow-TODO-strictly / no self-invented scope" directive — recorded here only
> because the user opted in explicitly. Revisit at the Fase-2 go/no-go before committing capital.

### venues — decodable adapters + bit-exact quoters (each gated by M1-GATE-EXT)
- [ ] ★ `onchain-17` Meteora DLMM swap adapter (constant-sum bins, bin-array accounts) *(onchain · 4d · onchain-3,4,5)*
- [ ] ★ `onchain-18` Meteora DAMM v2 / CP-AMM swap adapter (Token-2022 fee path) *(onchain · 3d · onchain-3,4,5)*
- [ ] ★ `onchain-19` Raydium CLMM swap adapter (sqrtPriceX64 Q64.64, tick arrays; reuses add-6 resolver pattern) *(onchain · 3d · onchain-3,4,5)*
- [ ] `sizing-12` Meteora DLMM quoter (bit-exact, active-bin walk + cross) *(sizing · 3d · sizing-3)*
- [ ] `sizing-13` Meteora DAMM v2 quoter (bit-exact, constant-product) *(sizing · 1.5d · sizing-3)*
- [ ] `sizing-14` Raydium CLMM quoter (bit-exact, in-range tick linearization) *(sizing · 2.5d · sizing-3, add-6)*
- [ ] `detection-11` Decoders + cache wiring for DLMM / DAMM v2 / Raydium CLMM (field offsets vs IDL) *(detection · 3d · detection-3)*
- [ ] ★ `M1-GATE-EXT` **GATE**: per-venue both-dir differential GREEN for the 3 new venues (extends M1-GATE; LiteSVM + Surfpool) *(testing · 4d · onchain-17,18,19, sizing-12,13,14)*

### triangle — 3-leg execution (changes core program + math shape)
- [ ] ★ `onchain-20` N-leg processor: snapshot → CPI A → CPI B → CPI C → terminal assert (generalize 2-leg `onchain-8`) *(onchain · 4d · onchain-8)*
- [ ] `sizing-15` Triangle per-leg re-size on the Bellman-Ford cycle (promotes `sizing-10` seam to active) *(sizing · 3d · sizing-10)*
- [ ] `detection-12` Negative-cycle discovery wired to the live pair graph (promotes detection seam) *(detection · 2.5d · detection-6, sizing-15)*
- [ ] `txbuilder-15` 3-leg route layout + account budget (locks<128, bytes≤1232 across 3 venues) *(txbuilder · 2.5d · txbuilder-5)*
- [ ] ★ `testing-11` Triangle differential + revert-on-unprofitable proof (LiteSVM + Surfpool) *(testing · 4d · onchain-20, M1-GATE-EXT)*

---

## ⚠️ Addendum — must-add BEFORE first mainnet capital (§9.1)

The completeness pass found these load-bearing items unowned in the DAG. Items (1)–(4) block a *safe* first land on the hot-pool / fresh-launchpad niche.

- [x] `add-1` **BLOCKER** · Fase 2 · In-flight writable-account registry + one-inflight-per-pool dedupe *(executor + detection hook)* — `WritableAccountRegistry` (atomic acquire, RAII release) gates the 2nd opp on a pool as `DropCause::WritableContention`; runs in `Executor::land` before signing
- [ ] 🟡 `add-2` **HIGH** · Fase 1 · Inventory round-trip-closure invariant (leg-B out mint == leg-A in/base mint) *(onchain + signer + testing)* — signer-side done (`TxShapeValidator` rejects `RouteDoesNotCloseToBaseAta`); 🔒 on-chain assert + LiteSVM negative test pending build-sbf
- [ ] `add-3` **HIGH** · Fase 2 · Route-specific SELL-simulation honeypot/rug gate (non-Jupiter) *(txbuilder vetting)* — feeds observ E[rug/honeypot]; necessary-not-sufficient (on-chain assert remains final net)
- [ ] `add-4` **HIGH** · Fase 1 · Live per-epoch Token-2022 fee read (`getEpochFee`) + epoch-boundary refetch *(detection on-demand RPC / txbuilder)* — fetch per opp per Token-2022 mint; stale = hard reject (sizing-2 only *enforces* staleness today)
- [ ] 🟡 `add-5` **MED** · Fase 1 · Runtime SIMD-0268/0339 feature-gate detection + measured CU-per-CPI budget *(onchain)* — `arb_config::features` (`FeatureGateState`/`CpiBudget::from_features`) owns the 5/9·1000/946·128/255 mapping; default = conservative pre-activation. 🔒 the runtime activation READ (RPC getFeatureActivation / on-chain feature set) + measured CU/CPI on LiteSVM still pending build-sbf/runtime
- [x] `add-6` **MED** · Fase 1 · Whirlpool tick-array / oracle on-demand resolver (quote + CPI account list) *(detection / txbuilder)* — `txbuilder/whirlpool.rs`: `start_tick_index` (floors toward −∞ via div_euclid), `tick_array_pda`/`oracle_pda`, `resolve_swap_accounts` (3 arrays in swap dir + oracle); >1-tick-cross → `CrossesTick` (Fase 3)
- [ ] `add-7` **SEAM** · Fase 3 · Phoenix CLOB partial-fill / IOC-FOK forward-seam contract only *(sizing + onchain adapter)*

### Decisions to close (§9.2)

- [ ] `dec-1` · Fase 0 · **Single concrete Agave / Yellowstone / SIMD pin** in `scaffold-3` `[workspace.dependencies]` (modules named 1.18.x / 2.1.0 / 3.x — must be ONE; drives SIMD feature-gate behavior). *Close in Fase 0.*
- [x] `dec-2` · Fase 2 · **CapReservation lifecycle across the landing rebuild loop** — `PreSignCaps::release` + `dec2_n_rebuilds_consume_one_count_slot` test: release-before-reserve in the same window epoch ⇒ N rebuilds = 1 count slot
- [x] `dec-3` · Fase 1 · **Single `min_profit` definition** (base==WSOL asymmetry) — pinned in `arb_types::CostTerms { swap_fees, priority, base_fee, tip, margin }.min_profit()`, shared by sizing / tx-builder / on-chain assert. (Open: whether to also read signer lamport delta on-chain — deferred to onchain.)

### Sequencing fix (§9.3)
- [ ] Collapse `onchain-10` + `sizing-9` + `testing-5` into ONE `M1-GATE` work-item, single owner: build the LiteSVM CPI harness exporting `realized_out(pool, amount_in, dir)` **once**, consumed by both off-chain and cross-module sides. Enforce **`M1-GATE green` as a hard Fase-2 entry gate** (add the release rule — no direct DAG edge exists).

### Lower-severity review notes (§9.4)
- [ ] Profit-assert account-budget itemized in LimitReport (extend txbuilder-6 / onchain-9): assert balance-read accounts are already-swap-loaded ATAs (marginal), not net-new loads
- [ ] jitodontfront asserted at bundle index 0; non-index-0 placement rejected pre-send (extend landing-4)
- [ ] 🟡 Confirmation-rank `(slot, index_in_block)` capture source pinned in observ-2 / landing-9 (getBundleStatuses or off-hot-path getBlock); assert populated, not defaulted — `ConfirmationRank` + `LatencyBook::last_confirmation()` returns `None` until captured (asserted, not defaulted to 0/0); the network SOURCE (getBundleStatuses) is seamed

---

## Critical path (longest chain to first profitable land)

`scaffold-1 → -2 → -3 → -4` → `sizing-1 → -3 → -4 → -7 → -8` → `onchain-1 → -2 → -6 → -8 → -9 → -10` → **`testing-5` (M1-GATE)** → `txbuilder-5` → `landing-2 → -3 → -4 → -5 → -6 → -7 → -8 → -9`

## Integration milestones (go/no-go checkpoints)

- [ ] 🟡 **M0** Workspace foundation green — scaffold-3,4,7,9,10 *(blocked only on scaffold-7 Solscan verification)*
- [ ] 🟡 **M0.5** Skeleton builds + LiteSVM revert proven — onchain-1,8,9, scaffold-11, testing-1 — **build-sbf works + LiteSVM revert GREEN**; CU/Token-2022 negatives residual
- [ ] 🟡 **M1-GATE** Rounding-mirror gate — **THE hard go/no-go** — **CP path GREEN in LiteSVM (2026-06-22)**; NOT fully closed: needs Surfpool real-venues + Token-2022 + both-dirs/Orca before mainnet capital — sizing-8, onchain-9,10, sizing-9, testing-5
- [ ] 🔒 **M1.5** Atomic tx builds + reverts on real programs (fork) — txbuilder-5,6,9,12, onchain-11, testing-8
- [ ] ⚠️ **NICHE-COVERAGE go/no-go (added 2026-06-22)** — before Fase-2 capital, confirm Wave-1 (Raydium CPMM + Orca Whirlpool + PumpSwap) actually reaches a live opportunity stream. On-chain audit evidence: pump.fun graduations land on PumpSwap (✓ Wave-1) but the deepest mispricings observed (ANB) were **intra-Meteora (DAMM v2 ↔ DLMM)** and the dex-to-dex sample's sell leg was **Raydium CLMM** — both outside the *original* Wave-1 set. Decision: either accept the narrower CPMM/Whirlpool/PumpSwap slice for first-land, or gate first-land behind Fase 2.5 venues. See `plan.md` §4 + §10.
- [ ] **M2** First profitable mainnet land via Jito — landing-4,6,8,9, signer-8, observ-14
- [ ] **M2.5** Deploy posture + verifiable build published — scaffold-12, onchain-13, signer-10,11

---

*Acceptance criteria per task: `implementation-plan.md` §5. DoD→task mapping: §11. Risk register: §10.*
