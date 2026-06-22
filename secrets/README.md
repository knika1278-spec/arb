# `secrets/` — gitignored key material (contract)

**Nothing real in this directory is ever committed.** `.gitignore` ignores everything
under `secrets/**` except this `README.md` and `.gitkeep`. Verify before adding anything:

```bash
# Must list NEITHER of these:
echo '{}' > secrets/hot-keypair.json && git status --porcelain | grep secrets
```

## Contract

| File | Purpose | Perms | Committed? |
|---|---|---|---|
| `hot-keypair.json` | Low-balance HOT key the signer sidecar loads (invariant §12). | `0o600` | **never** |
| `kill_switch` | Presence ⇒ signer refuses to sign / start (invariant §13). | — | **never** |
| `README.md`, `.gitkeep` | This contract. | — | yes |

## What is NOT here

- **Treasury / program-upgrade authority** lives in **KMS + Squads multisig**, never on
  disk in this repo (invariant §12, §14). `infra/scripts/gen-program-keypair.sh`
  deliberately does not generate it here.

## Generating the hot key

```bash
bash infra/scripts/gen-program-keypair.sh   # writes secrets/hot-keypair.json chmod 600
```

The only sanctioned loader is `arb_config::secrets::load_hot_keypair`, which **refuses**
a keyfile whose mode is not `0o600` on unix.
