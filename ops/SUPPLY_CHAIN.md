# Supply-chain posture

The single most important risk in this project is a **funded key handled by untrusted code**
(`plan.md` §9). The documented `Solana-Arbitrage-Bot` malware cluster (SlowMist) and the
`@solana/web3.js` 1.95.6/1.95.7 trojan (CVE-2024-54134) are the cautionary precedents.

## Controls in this repo

| Control | Where | Gate |
|---|---|---|
| Exact-pinned solana/spl/bytemuck/uint deps | root `Cargo.toml` `[workspace.dependencies]` | `cargo build --locked` |
| Committed `Cargo.lock` (checksums) | repo root | CI `git diff --exit-code Cargo.lock` |
| Known-vuln / yanked scan | `cargo audit` | CI `make audit` |
| Source + license + wildcard policy | `infra/deny.toml` | CI `cargo deny check` |
| Git-pinned dep integrity hashes | `infra/audit/integrity-hashes.lock` | CI `verify-integrity.sh` |
| Secrets never in git | `.gitignore` (`secrets/**`), `secrets/load_hot_keypair` 0o600 check | `gen-program-keypair.sh` guard |
| Reproducible on-chain bytecode | `solana-verify` | `verifiable-build.yml` (stable hash ×2) |
| Hot key isolated, small balance | `arb-signer` sidecar | runtime |
| Upgrade/treasury authority off-host | KMS + Squads multisig | `ops/runbooks/deploy_upgrade.md` |

## Two distinct threat models (do not conflate)

1. **Dependency malware** → mitigated by dep-pinning, lockfile, integrity hashes, cargo-deny,
   sandboxed execution with a throwaway key.
2. **Opsec / treasury compromise** (e.g. Step Finance ~$40M, device/social-engineering) →
   mitigated by multisig + hardware-isolated treasury signing + device hygiene. Dep-pinning
   does **not** help here.

## Rules

- Never fork/run any `Solana-Arbitrage-Bot`-style repo with a funded key. Study only the
  audited repos in `plan.md` §13.
- Adding a git-pinned dep requires, in the same PR: the rev in `integrity-hashes.lock` **and**
  the host in `infra/deny.toml [sources].allow-git`.
- Never weaken `tip_inside_tx` or the Token-2022 HARD-REJECT filter to "capture more volume"
  without an explicit risk decision recorded in the plan.
