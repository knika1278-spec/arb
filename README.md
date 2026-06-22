# arbit — Solana Atomic Arbitrage (Milestone 1)

Single-transaction, all-or-nothing atomic arbitrage between two fully-decodable
constant-product venues, with an **on-chain profit-assertion** that makes the runtime
revert the whole transaction when the trade is not profitable. Pre-funded WSOL/USDC
inventory (no flash loan). Wave-1 venues: **Raydium CPMM**, **Orca Whirlpool**, and
**PumpSwap AMM** (added in Fase 2).

- **Design spec (what & why):** [`plan.md`](./plan.md) — canonical, in Bahasa Indonesia.
- **Build plan (how & in what order):** [`implementation-plan.md`](./implementation-plan.md)
  — 9 modules, 107-task DAG, M1-GATE.

## ⚠️ Security warning — read before running anything

There is a documented cluster of **wallet-draining malware** repos on GitHub named some
variant of **`Solana-Arbitrage-Bot`** (authors incl. ChangeYourself0613, OnlyForward0613,
senior106, znjqolf, AV1080p, WSOL12, keidev-sol, kelvin-1013 — analysis by SlowMist).
They scrape `PRIVATE_KEY` from `.env`, bs58-obfuscate an exfil URL, and POST your key to
an attacker. **Never fork or run any of them with a funded key.**

- Study only the **audited** repos listed below.
- Pin every dependency + commit the lockfile + record integrity hashes (`infra/deny.toml`,
  `infra/audit/`). The `@solana/web3.js` 1.95.6/1.95.7 trojan (CVE-2024-54134) is the
  reason TypeScript prototypes must use `>=1.95.8`.
- Run everything in a sandbox with a throwaway, low-balance hot key.

## Workspace layout

```
arbit/
├── crates/            # SHARED source of truth (linked by on-chain AND off-chain)
│   ├── arb-config/    #   program-id allowlist, hard limits, provider ladder, secrets
│   ├── arb-types/     #   ArbError (repr u32), DexKind
│   └── arb-math/      #   bit-exact integer math (re-exports bot::math)
├── onchain/           # native-Rust TryArbitrage program (hot path; NOT Anchor)
├── bot/               # off-chain hot path
│   ├── arb-bot/       #   math, sizing, detection, txbuilder, executor, signer, metrics
│   └── arb-signer/    #   isolated signer-sidecar binary (only the hot key)
├── infra/             # config TOMLs, ALT tooling, supply-chain (deny/audit), scripts
├── analytics/         # backtest / golden-replay CLI
├── ops/               # deploy/runbooks (Squads multisig, kill-switch recovery)
├── secrets/           # gitignored key material (README contract only is committed)
└── tests/             # cross-module integration harness (LiteSVM, Surfpool, M1-GATE)
```

## Quickstart

```bash
make bootstrap     # install + version-verify the pinned toolchain (infra/toolchain/versions.toml)
make build         # cargo build --workspace --locked  (+ build-sbf for onchain)
make config-check  # loader::validate + program-id ⇄ toml cross-check
make test          # host unit/property tests (math + sizing M1-GATE math)
```

> **Toolchain note (this environment):** the host toolchain is rustc **1.96.0** and the
> Solana crates resolve to **2.3.x**, which is what the pins target and what is verified to
> compile here. `plan.md` §8 names 2.1.0/1.79.0 as aspirational; the concrete pin is an
> open decision (scaffold open-questions) to reconcile against the Agave platform-tools
> version at deploy time. The on-chain `build-sbf` step requires `solana-cli` /
> platform-tools (not installed here) — see `onchain/README.md`.

## Audited study repos (NOT malware)

- `buffalojoec/arb-program` — native-Rust atomic arb skeleton, `NoArbitrage` revert, ALT.
- `0xNineteen/blog.md` (rust-macros-arbitrage) — profit-revert + per-DEX CPI macros.
- `raydium-io/raydium-cpi-example`, `orca-so/whirlpool-cpi-sample`, `MeteoraAg/cpi-examples`.
- `raydium-io/raydium-amm` (`instruction.rs`/`processor.rs`/`math.rs`) — authoritative account counts + integer math.
- `raydium-io/raydium-clmm` (`swap_math.rs`/`full_math.rs`) — CLMM tick math.
- `rpcpool/yellowstone-grpc`, `jito-labs/shredstream-proxy`, `jito-foundation/geyser-grpc-plugin`.
- `Ellipsis-Labs/phoenix-v1`, `bcc-research/CFMMRouter.jl` (sizing math, Fase 3).

See `plan.md` §13 for the full reference list.
