# Chainstack API reference — for `arbit` (Solana atomic-arbitrage)

> Curated, **fact-checked** extract of the Chainstack Solana docs, scoped to this repo's TODO.md.
> Companion to `endpoint/meteora-api.json`. Source of truth = `https://docs.chainstack.com`
> (the bare `llms-full.txt` only serves a changelog slice; the real pages are linked per section).
>
> **Provenance / confidence.** Every fact below was fetched from a `docs.chainstack.com` page and
> independently re-verified (auth model + rate limits = *confirmed*; Yellowstone gRPC =
> *partially-correct*, with doc-vs-tutorial attribution called out inline). Tags:
> - **[doc]** quoted verbatim from a `docs.chainstack.com` page.
> - **[tutorial]** only in `github.com/chainstacklabs` example code, **not** the docs — treat as a
>   strong hint, confirm against the proto/console before relying on it in the hot path.
> - **[console]** per-node value that is *not* in public docs — read it from the Chainstack console
>   (project → network → node → Access & credentials / gRPC endpoint).
>
> Last verified: 2026-06-23.

---

## 0. Live validation — **all three transports PASS (2026-06-23)**

Real calls against the provisioned node (`solana-mainnet.core.chainstack.com`), not just config parsing:

| Transport | Auth | Result |
|---|---|---|
| **JSON-RPC (HTTPS)** | Basic Auth, bare host | ✅ `getHealth=ok`; `getVersion`=solana-core **4.1.0-beta.2**; `getSlot`, `getEpochInfo` (epoch 991), `getLatestBlockhash`, `getRecentPrioritizationFees` all **HTTP 200** (~0.3 s) |
| **WebSocket (WSS)** | Basic Auth, bare host | ✅ `slotSubscribe` acked + live `slotNotification` |
| **Yellowstone gRPC** | `x-token` metadata | ✅ TLS `h2` negotiated; `GetVersion` → geyser **13.3.0 / proto 12.5.0**; `Subscribe` streamed a live slot |

- **Auth model confirmed end-to-end:** Basic Auth (username/password) for RPC+WSS, `x-token` for gRPC —
  exactly what `DataSourceConfig::resolve_basic_auth` / `GrpcEndpoint::token_env` assume.
- **Pin for `detection-5`:** the server runs **yellowstone-grpc-geyser 13.3.0 (proto 12.5.0)** → pin the
  `yellowstone-grpc-client` Rust crate to the matching 13.x/proto-12.x line.
- Method: `curl` (RPC) · python `websockets` (WSS) · python `grpcio` against the compiled geyser proto
  (`GetVersion` unary + `Subscribe` stream).
- **Re-run anytime:** `bash tests/scripts/chainstack_smoke.sh` (RPC = authoritative/non-zero-exit on
  failure; gRPC TLS+h2 reachability + WSS slot stream = informational). Reads `.env` (CRLF-safe).

---

## 1. Authentication model (the `.env` reconciliation) — **confirmed**

A single Chainstack node exposes **two credential sets for JSON-RPC/WSS** (pick one *per URL*) plus a
**separate gRPC token**. There is no Bearer-token auth for node APIs.

| Scheme | Form | Used for | This repo's env var |
|---|---|---|---|
| **Key-in-path** | `https://nd-xxx.p2pify.com/<KEY>` · `wss://ws-nd-xxx.p2pify.com/<KEY>` | JSON-RPC + WSS | `CHAINSTACK_SOLANA_RPC_URL`, `CHAINSTACK_SOLANA_WSS_URL` |
| **Basic Auth** | `https://<USER>:<PASS>@nd-xxx.p2pify.com` (userinfo before `@`) · or `curl -u USER:PASS` | JSON-RPC + WSS (same node, *alternative* to key-in-path) | `CHAINSTACK_USERNAME`, `CHAINSTACK_PASSWORD` |
| **x-token** | `x-token: <token>` **gRPC metadata header** (NOT in URL) | Yellowstone Geyser gRPC | `CHAINSTACK_GRPC_TOKEN` |

- **[doc]** *"Chainstack offers two sets of credentials to access a node. One is via endpoints
  incorporating the API key directly in the URL, and the other is through endpoints requiring a
  username and password for access."* → `CHAINSTACK_USERNAME`/`PASSWORD` are **the same RPC node's
  Basic-Auth alternative**, not a second endpoint.
- ⚠️ **This repo's provisioned node uses form B (Basic Auth).** The live `.env` has
  `CHAINSTACK_SOLANA_RPC_URL = https://solana-mainnet.core.chainstack.com` — a **bare host with no key
  and no embedded `user:pass@`** — so the username/password are **REQUIRED, not redundant**: a bare-host
  request without them is **401**. The client applies them as `Authorization: Basic`
  (`DataSourceConfig::resolve_basic_auth`); Surfpool/`curl` can instead embed `user:pass@host`.
  (Use form A *or* B per node, never both.)
- **[doc]** gRPC uses `x-token` **in request metadata**, *"rather than URL-based auth tokens"*.
  Basic-Auth on gRPC appears **[tutorial]** only — use `x-token`.
- **[doc]** *"bearer token authentication is currently unavailable when it comes to blockchain APIs."*
  Do **not** send `Authorization: Bearer …` to the node. (Bearer is only for the platform API
  `api.chainstack.com`.)
- **[console]** the exact Solana host string (`nd-xxx.p2pify.com` vs `solana-mainnet.core.chainstack.com`)
  is per-node; the *schemes* above are universal. Solana CLI derives WSS by swapping protocol but
  **keeps the port — wrong for Chainstack**; set the `ws-nd-` host explicitly.

Docs: `/docs/authentication-methods-for-different-scenarios`, `/docs/manage-your-node`.

---

## 2. JSON-RPC + WebSocket — methods this bot uses

Endpoint form (dedicated node): `https://nd-xxx.p2pify.com/<KEY>` (POST, JSON-RPC). All **[doc]**:

| Method | Status | Arb-relevant note |
|---|---|---|
| `getAccountInfo` | supported | Single-account state clone; use `base64` for raw pool deser (`jsonParsed` only for token accts). |
| `getMultipleAccounts` | supported | Batch state reads; `base58` limited to <129 B data → use `base64`. Max-per-call **not** doc'd (Solana core = 100, **unverified** on Chainstack). |
| `getProgramAccounts` | **paid-only, filtered-only** | Unfiltered calls to large programs → `-32602`; `dataSize`/`memcmp` mandatory. **3 RPS** (global1/nyc1), 10 RPS (lon1). SPL-Token + Kin program IDs are **excluded from indexing**. ⇒ never on the hot path; precompute pubkeys + `getMultipleAccounts`. |
| `getEpochInfo`, `getEpochSchedule` | supported | Current epoch index — needed to pick the active Token-2022 transfer-fee (see `add-4`). |
| `getFeeForMessage` | supported | Per-message fee. |
| `simulateTransaction` | supported | `replaceRecentBlockhash` (true ⇒ uses bank's latest blockhash; **conflicts with `sigVerify`**), `sigVerify`, `accounts` (post-sim account state). Use for CU estimate + dry-run profit check. |
| `getRecentPrioritizationFees` | supported | See §6 — returns a **floor**, not a competitive fee. |
| `sendTransaction` | supported | `base64` (base58 deprecated), `skipPreflight`, `preflightCommitment` (default `finalized`), `maxRetries`. Warp add-on available (§6). |
| `requestAirdrop`, `voteSubscribe` | **unavailable** | (irrelevant to mainnet execution). |

- **Archive** (`getBlock`, `getTransaction`, `getSignaturesForAddress`, …) is **mainnet-only**; state
  methods (`getAccountInfo`/`getMultipleAccounts`/`getProgramAccounts`) are **not** archive — they
  serve current state from a full node. RU billing: **1 RU full / 2 RU archive** **[doc]**.
- WS subscription method names (`accountSubscribe`/`programSubscribe`/`slotSubscribe`) are standard
  Solana but **not enumerated** in the fetched pages — **unverified** here (Yellowstone gRPC is the
  recommended production stream anyway, §4).

Docs: `/docs/solana-methods`, `/reference/solana-*`, `/docs/solana-getaccountinfo-getmultipleaccounts`.

---

## 3. Rate limits & quotas — **confirmed** (design the bot around these)

- **[doc]** **Solana has its own, lower global RPS** than the generic table: mainnet **Developer = 5
  RPS, Growth = 50 RPS** (10× lower than the generic Developer 25 / Growth 250). Devnet matches the
  generic numbers. ⇒ on `first_profit`/Growth the **50 RPS** cap is the binding constraint; the bot
  must rate-limit its own RPC and lean on the gRPC stream for state, not on RPC polling.
- **[doc]** Per-method caps (apply on top of plan RPS): `getBlock` 400, `getBlockTime` 500,
  `getTokenAccountsByOwner` 80, `getTokenSupply` 300, **`getSupply` 2**, **`getProgramAccounts` 3**
  (global1/nyc1) / 10 (lon1), `getLargestAccounts` 0 (dedicated-only). The dangerous ones for an arb
  bot are `getProgramAccounts` (3) and `getSupply` (2).
- **[doc]** Request body cap **1 MB** → HTTP **413**; no documented hard cap on batch *count* (only the
  1 MB body bounds it). Exceeding RPS/connection limit → HTTP **429**.
- **[doc]** RU quota overage does **not** hard-stop — overage is auto-billed. Nodes auto-delete after
  **30 days** inactivity; org suspended after **60 days** inactivity.
- **[unverified]** per-plan **monthly RU pool sizes** + overage $/RU are **not** on docs (deferred to
  `chainstack.com/pricing`); do not hardcode them.

Docs: `/docs/limits` (Throughput guidelines), `/docs/rps-plan-limits`, `/docs/request-units`,
`/docs/quotas`, `/docs/error-reference`.

---

## 4. Yellowstone Geyser gRPC — the `detection-5` ingest contract

Endpoint **[console]** `<chain>-mainnet.core.chainstack.com:443` (port 443; pattern **[doc]** via
TRON/Sui examples — exact Solana host from the node Overview → gRPC). Auth = `x-token` metadata.

**SubscribeRequest** top-level **[doc]** (Node.js page):
`{ accounts, slots, transactions, transactionsStatus, entry, blocks, blocksMeta, commitment, accountsDataSlice }`
- `transactions.<name>` sub-object **[doc]**: `{ vote, failed, accountInclude, accountExclude, accountRequired }`
  — filter txs by program via `accountInclude: [<programId>, …]`.
- `accounts.<name>` map exists; filtering by **owner / memcmp / datasize** lives in the standard
  `rpcpool/yellowstone-grpc` proto (`SubscribeRequestFilterAccounts { account[], owner[], filters[] }`,
  `filters[]` = `memcmp{offset,bytes}` | `datasize`) — **[tutorial]/proto, not in Chainstack docs**;
  confirm field names against the proto. This is how the bot subscribes to **pool accounts by program
  owner** (Raydium CPMM / Orca Whirlpool / PumpSwap) → `RawAccountUpdate { pubkey, owner, data, slot,
  write_version }`.
- **Commitment** `PROCESSED=0 / CONFIRMED=1 / FINALIZED=2` **[tutorial]** — matches `commitment="processed"`
  in `providers.toml` (lowest latency; reverted-slot risk OK since arb txs revert atomically).
- **Keepalive [doc]**: send a ping every **10 s** (`PING_INTERVAL_MILLISECONDS = 10000`,
  `{ ping: { id: 1 }, accounts: {}, slots: {}, transactions: {} }`). Python channel keepalive opts are
  **[tutorial]**.

**Add-on tier limits [doc]** (`/docs/yellowstone-grpc-geyser-plugin`):

| Tier | Concurrent streams | Accounts/stream | Notes |
|---|---|---|---|
| $49/mo | **2** | **50** | matches `max_streams = 2` ⇒ up to **100 accounts** |
| $149/mo | 7 | 50 | needed if >100 accounts |
| $449/mo | 25 | 50 | — |

- **50 accounts/stream on every tier** — only the *stream count* scales (not accounts/stream).
- **5 concurrent filters of the same type** per connection (accounts/slots/transactions/…).
- **Cannot stream** `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` (SPL-Token) or Kin
  `kinXdEcpDQeHPEuQnqmUgtYykqKGVFq6CeVX5iAHJq6`.
- **Mainnet-only** (no devnet Geyser). Dedicated nodes allow limit customization.
- **WS-stream footgun [doc]**: Solana frames routinely >1 MiB (up to 10 MiB) → raise the client
  max-frame size or you eat close code **1009 (MESSAGE_TOO_BIG)** on the first big block. Relevant to
  `detection-7` reconnect/replay.
- Client crate: Node.js `@triton-one/yellowstone-grpc` **[doc]**; **Rust `yellowstone-grpc-client`**
  is the de-facto crate but **not named in Chainstack docs** — **[unverified]** (it's the rpcpool/Triton
  crate the service targets; the seam in `detection/grpc.rs` already assumes it).
- **[doc]** Chainstack Solana nodes include **Jito ShredStream by default** → faster slot reception,
  "especially beneficial with the Yellowstone gRPC plugin".

Docs: `/docs/yellowstone-grpc-geyser-plugin`, `/docs/solana-listening-to-programs-using-geyser-and-yellowstone-grpc-node-js`.

---

## 5. Transaction landing & fees

**`getRecentPrioritizationFees` [doc]** — the tip/priority-fee input for `landing-3`/`txbuilder-13`/`observ-4`:
- Param = array of up to **128 base-58 account addresses**; pass **writable state accounts** (pool
  state, reserves, user ATAs), **NOT program IDs** (*"Programs are executable and read-only … not
  present in the per-account writable fee map"*).
- Returns `prioritizationFee` **in micro-lamports per CU**, one entry per recent slot (150-slot window).
- The value is a **floor** — *"the cheapest fee that landed while writing to the most expensive of your
  accounts that slot — not a typical or competitive fee."* Recommended dynamic calc: **max non-zero /
  p95 over the window × ~1.5 urgency**. ⇒ the bot must bid **above** this floor; feed the tip cap math,
  don't treat it as the answer.
- Priority fee is set via `ComputeBudgetProgram.setComputeUnitPrice(microLamports)` (already modeled in
  `txbuilder-2`).

**Send path:**
- **[doc]** Blockhash valid **150 blocks (~80–90 s)**; expired → auto-reject ⇒ exactly the
  fresh-blockhash rebuild loop in `landing-6`. Use `base64`, low `maxRetries`, app-managed resend after
  the old blockhash expires (avoid dup).
- **[doc]** **Warp transactions / Trader Nodes** = Chainstack's reliable-send product: *all*
  `sendTransaction` routed to the current leader via a **staked validator / bloXroute** connection;
  all other calls stay on the normal node. **HTTP-only** (no WSS), **paid add-on**, billed per-tx,
  region-bound (~3–6 min to deploy). This is an **alternative** to `landing-7`'s Helius-Sender/SWQoS
  fallback — *not* in the current plan (plan.md §8/§10 uses Jito primary + Helius fallback), but the
  natural Chainstack-native option if a third reliable route is wanted. **Routing-exclusivity (§10)
  still applies** — don't double-broadcast a Jito-bundled tx.

Docs: `/reference/solana-getrecentprioritizationfees`, `/docs/solana-trader-nodes`, `/docs/warp-transactions`,
`/reference/solana-sendtransaction`, `/docs/solana-how-to-handle-the-transaction-expiry-error`.

---

## 6. Surfpool mainnet-fork datasource — `onchain-11` / `testing-8`

- **[doc]** Chainstack docs **reference Surfpool**: *"Surfpool wraps LiteSVM with a full JSON-RPC server
  and a copy-on-read mainnet fork"*, and *"Anchor 1.0 makes Surfpool the default validator for `anchor
  test` and `anchor localnet`."* (No doc shows the explicit Surfpool↔Chainstack-RPC wiring — that pairing
  is operational, see below.)
- **The wiring bridge:** `tests/scripts/run_surfpool.sh` reads **`SURFPOOL_DATASOURCE_RPC_URL`**, *not*
  `CHAINSTACK_SOLANA_RPC_URL`. To fork off the keyed Chainstack node, the operator sets:
  ```sh
  export SURFPOOL_DATASOURCE_RPC_URL="$CHAINSTACK_SOLANA_RPC_URL"   # key-in-path or user:pass@host
  ```
  (else it falls back to public `-n mainnet`). Lazy/copy-on-read fork ⇒ real Raydium CPMM / Orca
  Whirlpool programs + pool accounts appear on first reference — no snapshot needed.
- State cloning uses **current-state** methods (`getAccountInfo`/`getMultipleAccounts`), not archive.
  Bulk-cloning whole programs via `getProgramAccounts` is throttled (3 RPS) + filter-mandatory ⇒ prefer
  specific pubkeys + `getMultipleAccounts`.
- `solana-test-validator --url/--clone` is **not** Chainstack-documented (generic Solana tooling) — the
  Chainstack-blessed local-fork story is Surfpool.

Docs: `/docs/solana-tooling`, `/docs/solana-archive-nodes-…`, `/docs/solana-anchor-development`.

---

## 7. Chainstack → TODO.md task map (what each section unblocks)

| TODO task | What Chainstack provides | Action |
|---|---|---|
| **detection-5** (Yellowstone gRPC client) | §4 SubscribeRequest shape, `x-token` auth, `processed`, ping 10s, 100-acct budget, `yellowstone-grpc-client` | Implement `AccountUpdateSource` over the real client behind the existing `grpc.rs` seam; subscribe pool accounts by program `owner`; decode → `PriceView`. Heavy `tonic` dep = the only blocker. |
| **detection-9** (Fase-2 sub sizing 20–50 pairs) | §4 **50 accts/stream × 2 streams = 100 accounts** | A 2-venue pair = 2+ accounts; ~100-account budget ⇒ stay ≤ ~40 pairs on $49 tier, else $149 (7 streams). |
| **detection-3 / detection-11** (decoders) | §2 `getAccountInfo`/`getMultipleAccounts` `base64`; avoid `getProgramAccounts` | Verify field offsets via `getAccountInfo base64` on real pool accounts; never `getProgramAccounts` on hot path. |
| **add-4** (Token-2022 per-epoch fee) | §2 `getEpochInfo` (✓) — **note: Solana has no `getEpochFee`** | Read current epoch via `getEpochInfo`, then `getAccountInfo` the mint's `TransferFeeConfig` (older/newer fee by epoch); stale ⇒ hard reject (sizing-2 already enforces staleness). |
| **txbuilder-7 / landing-5** (preflight/sim gate) | §2 `simulateTransaction` (`replaceRecentBlockhash` ⊕ `sigVerify`, `accounts`) | CU estimate + post-sim balance profit check; honor the `replaceRecentBlockhash`/`sigVerify` exclusivity. |
| **landing-3 / txbuilder-13 / observ-4** (tip / priority fee) | §5 `getRecentPrioritizationFees` (floor, µlamports/CU, ≤128 writable accts) | Feed the floor into tip sizing as a **lower bound**; bid p95×1.5; pass pool/reserve/ATA pubkeys (not program IDs). |
| **landing-6** (fresh-blockhash loop) | §5 150-block (~80–90 s) blockhash window, base64, low maxRetries | Already seamed; this is the documented basis. |
| **landing-2 / landing-7** (Jito / Sender / SWQoS) | §5 Warp/Trader Node (bloXroute staked, HTTP-only, paid) | Optional Chainstack-native 3rd route; not in plan — keep Jito+Helius; respect routing-exclusivity. |
| **onchain-11 / testing-8** (Surfpool real-venue fork) | §6 copy-on-read fork off the keyed node | `export SURFPOOL_DATASOURCE_RPC_URL=$CHAINSTACK_SOLANA_RPC_URL` before `run_surfpool.sh`. |
| **detection-7** (reconnect/replay) | §4 WS 1009 / raise max frame; ShredStream default | Set a large max-frame on the gRPC client; `resubscribe_from_slot` already covers slot replay. |
| **observ / cost model** | §3 RU billing (1/2), 50 RPS Growth cap, 429 | Rate-limit RPC to <50 RPS; model RU cost; treat 429 as backpressure. |

---

## 8. Config wiring status (this branch)

Done in `worktree-arbit-chainstack-ref` (host-only, `arb-config` green: 24 tests, clippy, `config-check`):

- `GrpcEndpoint` gained a **`url_env` override + `resolve_url()`**, mirroring `json_rpc_url_env`, so the
  real **`CHAINSTACK_GRPC_URL`** reaches the client instead of the `yellowstone.chainstack.example:443`
  placeholder. (gRPC `x-token` stays in `token_env`.)
- `providers.toml [data_source.grpc]` now declares `url_env = "CHAINSTACK_GRPC_URL"`.
- `DataSourceConfig` gained `basic_auth_user_env`/`basic_auth_pass_env` + **`resolve_basic_auth() ->
  Option<(user, pass)>`** so the bare-host Basic-Auth node (form B) is consumable: the future RPC/gRPC
  client reads the creds (named by `providers.toml`) and sets `Authorization: Basic`. `providers.toml`
  declares `basic_auth_user_env="CHAINSTACK_USERNAME"`, `basic_auth_pass_env="CHAINSTACK_PASSWORD"`.
- `validate()` now rejects a configured gRPC endpoint with a blank `token_env`, and any blank declared
  indirection: `grpc.url_env`, `basic_auth_user_env`, `basic_auth_pass_env` (step-6/7 hardening).
- `.env.example` documents `CHAINSTACK_GRPC_URL` + the `CHAINSTACK_USERNAME`/`PASSWORD` Basic-Auth set
  with the verified auth model (form A key-in-path vs form B bare-host + Basic Auth).

**Residual (deferred RPC client):** `resolve_json_rpc()` returns the bare host as-is; the actual
`Authorization: Basic` header is applied when the real RPC client lands (the heavy network crate is the
deferred blocker, per `txbuilder-7`/`landing`). `resolve_basic_auth()` is the seam it consumes.

**Operator checklist (console-only — code cannot automate):**
1. Install the Yellowstone gRPC add-on; IP-allowlist.
2. From the node Overview, set in `.env`: `CHAINSTACK_GRPC_URL=<host>:443`, `CHAINSTACK_GRPC_TOKEN=<x-token>`.
3. Auth — pick ONE: **form A** `CHAINSTACK_SOLANA_RPC_URL=https://nd-xxx.p2pify.com/<KEY>` (blank USERNAME/PASSWORD),
   or **form B** (this node) `CHAINSTACK_SOLANA_RPC_URL=https://solana-mainnet.core.chainstack.com` (bare host)
   **+** `CHAINSTACK_USERNAME`/`CHAINSTACK_PASSWORD`.
4. For Surfpool forks: `export SURFPOOL_DATASOURCE_RPC_URL="$CHAINSTACK_SOLANA_RPC_URL"` (form B: embed
   `https://${CHAINSTACK_USERNAME}:${CHAINSTACK_PASSWORD}@<bare-host>`; mind the `.env` CRLF — `tr -d '\r'`).
5. `make config-check` must stay green.

## 9. Honest gaps (read from console / proto, do not hardcode)

- Exact **Solana host strings** for RPC, WSS, and gRPC (`*.p2pify.com` vs `*.core.chainstack.com`) — **[console]**.
- Yellowstone **accounts-filter `owner`/`memcmp`/`datasize`** field exact names — confirm against the
  `rpcpool/yellowstone-grpc` proto (docs don't show a populated example).
- Rust crate recommendation — `yellowstone-grpc-client` is de-facto, not doc-stated.
- Per-plan **monthly RU pool / overage rates** — only on `chainstack.com/pricing`, not docs.
- Max accounts per `getMultipleAccounts` on Chainstack (Solana core 100 assumed, not doc-confirmed).
