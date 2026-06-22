# Runbook — program deploy & upgrade (Squads multisig)

The on-chain program is deployed **upgradeable**, with the upgrade authority held by a
**Squads multisig** (invariant §14). CI produces and verifies the bytecode hash; it never
holds the authority. Deploy is a multisig-gated human step.

## Prerequisites

- Agave platform-tools installed (`make bootstrap` warns if `solana-cli` is missing).
- Program keypair at `secrets/program-keypair.json` (`infra/scripts/gen-program-keypair.sh`),
  and `declare_id!` + `infra/config/program_ids.toml` set to its pubkey.
- A green `make build-sbf` and a green M1-GATE (`tests/differential_rounding.rs`).

## Steps

1. **Reproducible build + hash**
   ```bash
   bash onchain/arb-program/build-verifiable.sh   # prints the executable hash
   ```
   Confirm the hash matches the `verifiable-build` CI artifact for the tagged commit.

2. **Initial deploy** (one-time), then transfer authority to the multisig:
   ```bash
   solana program deploy target/deploy/arb_program.so \
     --program-id secrets/program-keypair.json --upgrade-authority <DEPLOYER>
   solana program set-upgrade-authority <PROGRAM_ID> \
     --new-upgrade-authority <SQUADS_MULTISIG> --skip-new-upgrade-authority-signer-check
   ```

3. **Upgrades** are proposed + approved in Squads against a buffer account:
   ```bash
   solana program write-buffer target/deploy/arb_program.so   # -> <BUFFER>
   # Squads: propose `bpf_loader_upgradeable::upgrade` from <BUFFER>, collect approvals, execute.
   ```

4. **Verify on-chain** matches source:
   ```bash
   solana-verify verify-from-repo --program-id <PROGRAM_ID> <THIS_REPO_URL>
   ```

5. **Record** the deployed hash + tx in the change log.

> Until platform-tools are available in the build environment, steps 1–4 cannot run here;
> the program currently compiles for the host target only (see `onchain/README.md`).
