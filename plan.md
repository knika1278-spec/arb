# PLAN: Sistem Atomic Arbitrage di Solana

> Dokumen kanonikal untuk tim engineering. Milestone pertama: **ATOMIC arbitrage** (single-transaction, all-or-nothing, revert-if-unprofitable). Ditulis dalam Bahasa Indonesia; semua istilah teknis, nama library, program ID, dan code block tetap dalam English.

---

## 1. Ringkasan Eksekutif & Tujuan

### Apa yang kita bangun
Sebuah sistem **atomic arbitrage** di Solana: bot yang mendeteksi dislokasi harga antar DEX pool (mis. SOL lebih murah di Orca, lebih mahal di Phoenix), lalu mengeksekusi *seluruh* rangkaian swap di dalam **satu transaksi** yang bersifat all-or-nothing. Jika trade tidak profitable saat eksekusi on-chain, transaksi **revert** seluruhnya dan kita hanya membayar fee (base + priority), tanpa pergerakan token bersih, tanpa inventory yang nyangkut.

### Scope "atomic-first" (dan apa yang dengan sengaja TIDAK kita bangun dulu)
- **IN-SCOPE (Milestone 1):** Single-transaction on-chain arb antara 2 pool CPMM yang fully-decodable (**Raydium CPMM — bukan AMM v4 legacy — + Orca Whirlpool**), dengan on-chain profit-assertion yang me-revert bila tidak untung. Pre-funded inventory (WSOL/USDC), bukan flash loan. *(Raydium AMM v4 ditunda ke Wave 2 / Fase 3 — lihat §4; konsistenkan dengan Fase-1 "Scope ketat".)*
- **OUT-OF-SCOPE awal:** Non-atomic CEX/DEX latency arb (membawa inventory risk + hedging), sandwich/frontrun, flash loan, triangular/multi-hop kompleks, prop-AMM decoding. Semua ini masuk fase lanjutan.

> **SCOPE EXPANSION (added 2026-06-22, per permintaan eksplisit user setelah on-chain audit).** Atas dasar bukti
> mainnet (lihat §4 "Di mana arb sebenarnya ada") bahwa peluang terbesar berada di **Meteora DLMM/DAMM v2**,
> **Raydium CLMM**, dan **triangle**, item-item ini dipromosikan menjadi **aktif** di `TODO.md` → "Fase 2.5".
> Ini **memperluas M1 melampaui definisi "atomic 2-swap"**: triangle = N-leg (bukan 2-leg) dan butuh sizing
> cycle-based (Bellman-Ford), bukan closed-form. **M1-GATE tetap berlaku per-venue/both-direction** sebagai gate
> keras — tidak ada venue baru yang mainnet-eligible sebelum differential-nya GREEN (`M1-GATE-EXT`). Diakui
> bertentangan dengan directive "follow-TODO-strictly"; dicatat di sini hanya karena user opt-in eksplisit.

### Mengapa atomic dulu
Atomic intra-chain arb adalah **satu-satunya bentuk arb yang riskless setelah landing**: kedua leg di chain yang sama, di tx yang sama. CEX/DEX arb bersifat non-atomic — leg on-chain dan hedge off-chain settle terpisah, sehingga searcher harus warehouse inventory, menanggung inclusion risk dan price risk. Milestone atomic menghindari semua itu by design.

### Success criteria (Definition of Success milestone 1)
1. On-chain program (native Rust) dengan instruction tunggal `TryArbitrage`: 2 swap CPI + terminal profit-assert yang return `Err(Unprofitable)` bila `out_balance < in_balance + min_profit + costs`.
2. Terbukti revert pada input yang sengaja dibuat tidak profitable (di LiteSVM + mainnet-fork), tanpa pergerakan token bersih.
3. Mendarat **profitable** minimal sekali di mainnet pada size kecil melalui Jito bundle, dengan tip yang berada di dalam unit atomic yang sama.
4. Detection latency end-to-end terukur (Yellowstone gRPC), revert-rate terinstrumentasi sebagai metrik kesehatan utama.

### Ekspektasi realistis (jujur sejak awal)
Liquid-pair arb (SOL/USDC mayor) **tersaturasi dan secara praktis tidak bisa dimenangkan** tanpa infrastruktur co-located low-latency. Average profit per arb ~**$1.58** (mean berekor panjang, bukan ceiling). Edge nyata untuk pemain baru ada di **thin/fresh launchpad pools** dan memecoin pairs, di mana kompetisi ~2 orde lebih ringan. Kita membangun untuk belajar arsitektur dulu, lalu menargetkan niche, bukan mengalahkan top-3 bot di pair likuid.

---

## 2. Konsep: Apa itu Atomic Arbitrage di Solana

### Atomicity = properti runtime, bukan trik program
Solana mengeksekusi transaksi sebagai **satu unit**: jika *instruction* mana pun (termasuk trailing profit-assertion) me-return `Err`, **runtime** me-revert SELURUH state (semua swap leg, dan SOL transfer apa pun di dalam tx). On-chain assert hanyalah **gate** yang memicu `Err`; rollback dilakukan runtime (`RollbackAccounts`: hanya saldo fee payer yang terdebet dan nonce yang ditulis balik). Tidak ada opcode "revert" khusus — cukup `return Err` / `require_gte!` / `err!()`.

### Revert-on-no-profit: biaya saat gagal (presisi, sesuai fact-check)
Pada transaksi yang **gagal tapi sudah included** dalam block, biaya yang dibayar HANYA fee di muka:
- **Base fee 5,000 lamports/signature** (~0.000005 SOL), 50% dibakar, 50% ke validator. Non-refundable.
- **Priority fee** = `SetComputeUnitPrice (micro-lamports/CU) × requested CU LIMIT` (bukan CU actual yang dipakai). Non-refundable. **Over-request CU = overpay**, jadi set limit dari hasil simulasi + ~10% margin.

**Nuansa tip (KOREKSI penting):** Bila Jito tip ditempatkan sebagai SOL-transfer instruction **di dalam transaksi atomic yang sama**, maka saat assert return `Err`, tip transfer ikut ter-revert — jadi **tip TIDAK dibayar saat gagal**, hanya base + priority. Tip baru menjadi sunk cost bila tip berada **di luar** transaksi yang revert (mis. tx tip terpisah dalam bundle) ATAU bundle landing on-chain tapi leg lain gagal. **Rekomendasi: taruh tip di dalam tx arb yang sama** supaya gagal = tip tidak terbayar.

**Nuansa inclusion:** Fee hanya berlaku bila tx benar-benar masuk block (lolos sig verify, blockhash valid, fee payer solvent, account locks). Tx yang di-drop sebelum inclusion (bad blockhash, pre-check gagal, simulation-only) **biaya nol**.

### Tidak ada public mempool — deteksi via state streaming
Solana **tidak punya mempool ala Ethereum**. Gulf Stream meneruskan tx via gossip langsung ke leader berikutnya. Deteksi peluang karena itu lewat **streaming account/state**, bukan mempool sniping:
- **Yellowstone gRPC (Geyser)** untuk account/program writes (confirmed/processed) — namun emit hanya SETELAH node replay tx.
- **Jito ShredStream** untuk shred-level data sub-slot — memberi **tx intent** (bukan account state) lebih awal, bisa land di slot yang sama.

### Mengapa Solana cocok
Fee murah, block ~400ms, dan **composability** (CPI lintas Raydium/Orca/Meteora/Phoenix dalam satu tx). Atomicity gratis dari runtime + biaya gagal yang sangat kecil membuat strategi "spam-and-revert" frekuensi tinggi secara ekonomis viable.

### Jalur inclusion
Dimenangkan via (a) priority-fee + SWQoS staked connection, atau (b) **Jito bundles** (auction ranked by tip-per-CU). Jito-Solana puncak ~95% stake (awal 2025) lalu **turun ke ~85-90% dan terus menurun** seiring adopsi Frankendancer/Firedancer (~21% stake per Okt 2025). Bundle tetap jalur de-facto, **tapi leader Frankendancer TIDAK menghormati bundle** → makin banyak slot non-bundle yang harus diraih lewat priority-fee + SWQoS.

---

## 3. Arsitektur Sistem

```
                         ┌──────────────────────────────────────────────────────────┐
                         │                    DATA / INGEST PATH                      │
                         │                                                            │
   Mainnet pools  ──────▶│  Yellowstone gRPC (Geyser, processed)  ─┐                  │
   (Raydium/Orca/        │  Jito ShredStream (tx-intent, sub-slot) ─┤                 │
    Meteora/Phoenix)     │                                          ▼                 │
                         │                            ┌─────────────────────────┐     │
                         │                            │  POOL-STATE CACHE        │     │
                         │                            │  - keyed by pool pubkey  │     │
                         │                            │  - idempotent (slot +    │     │
                         │                            │    write_version)        │     │
                         │                            │  - decode AMM/CLMM/CLOB  │     │
                         │                            └───────────┬─────────────┘     │
                         └────────────────────────────────────────┼───────────────────┘
                                                                   ▼
                         ┌──────────────────────────────────────────────────────────┐
                         │                 STRATEGY / MATH LAYER                      │
                         │  1. Token-pair graph; recompute affected edges on update   │
                         │  2. DISCOVERY: Bellman-Ford/SPFA negative-cycle (direction)│
                         │  3. SIZING: CPMM closed-form (2-pool) / ternary search     │
                         │     (CLMM/DLMM); u128/u256, mul_div_floor, exact rounding  │
                         │  4. Gate: (spread - swap_fees - flash_fee - tip - prio) > 0│
                         └────────────────────────────────┬─────────────────────────┘
                                                          ▼
                         ┌──────────────────────────────────────────────────────────┐
                         │                   TX BUILDER LAYER                         │
                         │  - v0 VersionedTransaction + pre-warmed ALT(s)             │
                         │  - [ComputeBudget, (WSOL wrap), swap1, swap2,              │
                         │     profit_assert, jito_tip]                               │
                         │  - SetComputeUnitLimit (measured+10%), SetComputeUnitPrice │
                         │  - maxAccounts budgeting (<128 locks, <1232 bytes)         │
                         └───────────────┬──────────────────────────┬────────────────┘
                                         ▼                          ▼
                         ┌──────────────────────────┐   ┌──────────────────────────┐
                         │   PRE-FLIGHT SIM          │   │   SIGNER (sidecar)        │
                         │  simulateTransaction      │   │  in-process ed25519       │
                         │  (replaceRecentBlockhash, │   │  hot key (low balance),   │
                         │   sigVerify=false)        │   │  kill-switch flag check   │
                         └───────────┬──────────────┘   └──────────┬───────────────┘
                                     ▼                             ▼
                         ┌──────────────────────────────────────────────────────────┐
                         │                  EXECUTOR / LANDING PATH                   │
                         │  Jito Block Engine sendBundle (regional)  ──┐              │
                         │  OR Helius Sender (SWQoS + Jito dual-route)  │              │
                         │  poll getInflightBundleStatuses → getBundleStatuses        │
                         │  on no-land ~2-3s: REBUILD fresh blockhash, resubmit       │
                         └────────────────────────────┬─────────────────────────────┘
                                                      ▼
                         ┌──────────────────────────────────────────────────────────┐
                         │            ON-CHAIN ARBITRAGE PROGRAM (native Rust)        │
                         │  TryArbitrage: snapshot pre-balance → swap CPI A →         │
                         │  swap CPI B → assert(post >= pre + min_profit + costs)     │
                         │  else return Err(Unprofitable)  ⇒ runtime reverts ALL      │
                         └──────────────────────────────────────────────────────────┘
```

**Data flow ringkas:** Geyser/ShredStream → pool-state cache (idempotent) → strategy/math (discover + size) → tx builder (v0 + ALT) → pre-flight sim → signer → Jito/Sender → on-chain program (assert-or-revert) → status polling → metrics (revert rate, latency, PnL).

---

## 4. Cakupan Venue / DEX

### Tiga kelas venue
1. **AMM/CLMM decodable** (punya IDL, state bisa dibaca off-chain) — **fokus utama Milestone 1.** Bisa hitung dislokasi off-chain dan assert-or-revert on-chain dengan matematika deterministik.
2. **On-chain CLOB** — Phoenix, OpenBook v2. Settle atomic dalam satu tx; baca bid/ask ladder, bukan `x*y=k`.
3. **Proprietary "dark" oracle AMMs** (prop AMMs) — SolFi, HumidiFi, ZeroFi, Obric, GoonFi, Tessera. State opaque/undocumented (tanpa IDL publik); **jangan coba decode**. Akses hanya via Jupiter quote/route atau on-chain simulation.
   - **Lifinity v2 BUKAN dark AMM** (koreksi): open-source & decodable, ada SDK publik `@lifinity/sdk-v2` + docs. Ia *sulit di-arb karena oracle-priced* (harga di-track ke Pyth), **bukan** karena opaque. Boleh dibaca off-chain seperti AMM biasa, sekadar prioritas rendah untuk Wave 1.

### Prioritas integrasi (opinionated)
- **Wave 1 (MVP):** Raydium CPMM, Orca Whirlpool, **+ PumpSwap AMM** (lihat KOREKSI di bawah). Alasan: account-count efisien, math deterministik (constant-product), official CPI sample ada. **Hindari Raydium AMM v4 legacy** dulu karena berat akun.
- **Wave 2:** Raydium AMM v4 (untuk pool yang hanya ada di sana), Meteora DLMM + DAMM v2.
- **Wave 3:** Phoenix (CLOB leg — sumber dislokasi bersih), lalu prop-AMM via Jupiter route saja.

> **KOREKSI scope niche (penting):** Sejak ~Maret 2025, graduation pump.fun masuk ke **PumpSwap** (`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`), **BUKAN Raydium lagi** (95%+ grad). Jadi scope Raydium-CPMM + Orca **hanya menangkap minoritas** aliran memecoin segar — padahal itu **niche utama** (§10). Feeder Raydium fresh-pool yang masih hidup = **LaunchLab/Bonk.fun** (~150-285 pool/hari). **Untuk benar-benar memanen niche memecoin, masukkan PumpSwap AMM ke Wave 1** (constant-product mirip CPMM, CPI-able, decodable).

> ⚠️ **RISIKO COVERAGE NICHE (added 2026-06-22, bukti on-chain).** Audit 4 tx mainnet mengonfirmasi keras KOREKSI di atas: peluang **terbesar** yang teramati (ANB, 0.2→$696k & 0.1→$196k) semuanya **intra-Meteora (DAMM v2 ↔ DLMM)**, dan sample dex-to-dex (elun) leg-jual-nya **Raydium CLMM** — **semua di luar Wave-1 awal** (CPMM+Whirlpool+PumpSwap). Artinya Wave-1 menangkap irisan niche **lebih sempit** dari yang disiratkan. **Mitigasi:** Fase 2.5 (Meteora DLMM/DAMM v2 + Raydium CLMM + triangle) dipromosikan aktif di `TODO.md`, di belakang `M1-GATE-EXT`. **Keputusan go/no-go Fase-2:** terima irisan sempit untuk first-land, ATAU gate first-land di belakang Fase 2.5. Lihat checkpoint "NICHE-COVERAGE" di `TODO.md`.

### Program IDs kanonikal (2025-2026) — verifikasi on-chain sebelum wiring
| Venue | Program ID | Catatan |
|---|---|---|
| Raydium AMM v4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | authority `5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1` |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | |
| Raydium CPMM | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` | Token-2022-friendly; Pump graduations |
| Orca Whirlpool | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` | sqrtPriceX64 (Q64.64) |
| Meteora DLMM | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` | bin (constant-sum) |
| Meteora DAMM v2 / CP-AMM | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` | unique swap accounts; Token-2022 |
| Meteora Dynamic AMM v1 | `Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB` | |
| Lifinity V2 | `2wT8Yq49kHgDzXuPxZSaeLaH1qbmGXtEyPy64bL7aD3c` | oracle-AMM, **decodable** (public `@lifinity/sdk-v2`); sulit di-arb karena oracle-priced, bukan opaque |
| Phoenix v1 | `PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY` | CLOB, no crank |
| OpenBook v2 | `opnb2LAfJYbRMAHHvqjCwQxanZn7ReEHp1k81EohpZb` | CLOB |
| PumpSwap AMM | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | |
| SolFi | `SoLFiHG9TfgtdUXUjWAxi3LtvYuFyDLVhBWxdMZxyCe` | prop AMM (opaque) |
| ZeroFi | `ZERor4xhbUycZ6gb9ntrhqscUcZmAbQDjEAtCf4hbZY` | prop AMM (opaque) |
| Obric V2 | `obriQD1zbpyLz95G5n7nJe6a4DPjpFwa5XYPoNm113y` | prop AMM (opaque) |
| Jupiter v6 (aggregator) | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` | off-chain route builder |
| Token-2022 | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` | |
| Address Lookup Table | `AddressLookupTab1e1111111111111111111111111` | |

> **Catatan ID truncated:** HumidiFi (`9H6tu…`), Tessera (`TessV…`), GoonFi (`goonER…`) hanya tertangkap sebagian di sumber. **Treat sebagai unverified** — verifikasi di Solscan sebelum dipakai.

### CPI feasibility table (Milestone 1)
| Venue | Decodable off-chain? | CPI-able? | Account weight | Token-2022 hooks? | Rekomendasi Wave |
|---|---|---|---|---|---|
| Raydium CPMM | Ya (x*y=k) | Ya | Ringan | Fee ya, hook tidak | **1** |
| Orca Whirlpool | Ya (sqrtPriceX64) | Ya (swap_v2) | Sedang (tick arrays) | Fee + hook (swap_v2) | **1** |
| Raydium AMM v4 | Ya (reserves) | Ya | **Berat (17 akun; `amm_target_orders` deprecated)** | — | 2 |
| Meteora DLMM | Ya (bins) | Ya | Sedang-berat (bin arrays) | — | 2 |
| Meteora DAMM v2 | Ya | Ya | Sedang | Fee ya | 2 |
| Phoenix | Ya (ladder) | Ya | Sedang | — | 3 |
| Prop AMMs (SolFi dst.) | **Tidak** | Ya (via Jupiter) | — | — | 3 (route only) |

### KOREKSI account-count Raydium AMM v4 (authoritative)
- **SwapBaseIn = tag 9**, data `{ amount_in: u64, minimum_amount_out: u64 }`. **Tepat 17 akun kanonikal** (urutan: `token_program, amm_pool, amm_authority, amm_open_orders, amm_coin_vault, amm_pc_vault, market_program, market, market_bids, market_asks, market_event_queue, market_coin_vault, market_pc_vault, market_vault_signer, user_source, user_destination, user_owner(signer)`). **`amm_target_orders` sudah deprecated / di-comment-out** di `raydium-amm/program/src/instruction.rs` ("no longer used") — **JANGAN** perlakukan sebagai akun ke-18; tabel feasibility yang menulis "17-18 akun" sebaiknya dibaca **17**. Catatan: market accounts ini me-refer **OpenBook/Serum v1** (`srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX`), bukan OpenBook v2.
- **SwapBaseInV2 = tag 16**, **SwapBaseOutV2 = tag 17**: **8 akun** (`spl_token, amm_pool, amm_authority, amm_coin_vault, amm_pc_vault, user_source, user_destination, user_owner`) dengan menghilangkan semua akun market.
- V2 **tidak** "men-disable" orderbook; apakah pool consult OpenBook ditentukan runtime oleh flag `AmmStatus orderbook_permission`.
- **Budgeting:** ~17-18 akun per Raydium v1 leg vs 8 per v2 leg. **Prefer V2/CPMM/Whirlpool** untuk hemat akun.

### Di mana arb sebenarnya ada
~50% volume DEX Solana adalah arb; 2025 ada ~90.4M arb tx sukses via deteksi Jito, ~$142.8M profit. Dislokasi terdalam: SOL/USDC + stables di Orca/Raydium/Meteora, dan **cross-pool mismatch** (mis. token sama di DAMM v2 vs DLMM). Prop AMMs membawa mayoritas volume SOL/USDC (puncak ~86%) tapi opaque — kita tidak kompetitif di sana untuk Wave 1.

**Contoh nyata terverifikasi (audit Solscan 2026-06-22)** — semua v0-tx + ALT + ComputeBudget, single atomic tx via arb-program pihak ketiga (mengonfirmasi shape M1: atomic + tip-inside-tx). Daftar signature lengkap + kandidat golden-sample ada di `TODO.md` → "On-chain reference examples":
- **DEX-to-DEX** elun: PumpSwap AMM → Raydium CLMM, +3.35 SOL net, tip ix `jesterKqz…`.
- **Triangle** ANB: Meteora DAMM v2 ×2 → DLMM, 0.227→696,194 USDC, Jito tip 2.30 SOL **di dalam tx** (ix #5).
- **Internal-DEX** ANB: Meteora DAMM v2 → DLMM, via arb-bot `sattC…`; satu varian membayar **bribe 141 SOL** (bukti bribe-war = validasi cap-tip `txbuilder-13`).
- Ketiga jackpot ANB ada di **Meteora** (Wave-3 awal) → mendasari promosi Fase 2.5; lihat "RISIKO COVERAGE NICHE" di §4.

---

## 5. Lapisan Deteksi Peluang

### Feed primer: Yellowstone gRPC (Geyser)
- Library: `rpcpool/yellowstone-grpc` ("Dragon's Mouth", Triton One, AGPL-3.0; client Rust/TS/Go). Provider: Triton, Helius, Shyft, Chainstack, QuickNode.
- Subscribe `accounts` filter by `owner` (DEX program IDs) + `memcmp`/`datasize`/`tokenAccountState`. Pakai `accounts_data_slice {offset,length}` untuk potong payload.
- **Commitment `processed`** untuk latency terendah. Risiko reverted-slot dapat diterima karena tx atomic akan revert bila dislokasi sudah hilang.
- Latency (Triton-measured, p90): gRPC ~5ms slots / **~215ms accounts** vs native WS ~10ms / ~374ms.

### Feed sub-slot: ShredStream (upgrade path)
- `jito-labs/shredstream-proxy`: butuh keypair yang di-allowlist via Jito Discord, heartbeat, forward shreds ke `DEST_IP_PORTS` (default 20000/udp, no NAT, max 2 region).
- **KOREKSI penting:** Shred feed memberi **tx intent** (decode swap tx untuk antisipasi), **bukan account state**. Pakai shred untuk reaksi same-slot ("zero-slot"), dan gRPC untuk confirmed state.
- Helius LaserStream: decode shred ~8ms lebih cepat dari Yellowstone, 24h replay, 9 region. Bisa land di slot yang sama.

### Pool-state cache + token-pair graph
- In-memory cache keyed by pool pubkey. **Idempotent**: dedupe by `slot + write_version`, apply hanya `write_version` tertinggi per slot. **KOREKSI penting:** `write_version` adalah counter **global-monotonic per-validator/per-SESI**, *bukan* reset per slot dan **tidak comparable lintas reconnect / multi-node failover** (mis. Helius LaserStream pindah node). Bandingkan `write_version` **hanya di dalam sesi yang sama**; lihat Reconnect/replay.
- Decode per update: AMM = reserves (constant product); Raydium CLMM = `liquidity (u128)`, `sqrtPriceX64 (u128)`, `tickCurrent (i32)`; Orca Whirlpool = sqrtPrice Q64.64, tickSpacing, feeRate; Meteora DLMM = LbPair + BinArray (bin_step). Pakai `bytemuck` zero-copy (Rust) atau `BorshAccountCoder` (`@coral-xyz/anchor`). **Wajib:** Raydium CLMM / Orca Whirlpool / Meteora DLMM adalah program **Anchor** → buffer akun diawali **8-byte discriminator**; field struct mulai di **offset 8**, bukan 0 (verifikasi discriminator dulu, lalu cast dari offset 8). *(Beda dengan SPL token-account `amount` yang di offset 64 — itu bukan akun Anchor.)*
- Token-pair graph: hitung ulang hanya edge yang terdampak pada tiap update, bukan full re-scan.

### Strategi langganan: targeted vs owner-firehose + sizing pool (berapa pool dibutuhkan)
**Reframe:** yang menentukan tier gRPC bukan "jumlah pool" tapi **cara langganan**.
- **Targeted** (`account:[pubkey...]` eksplisit): presisi, bandwidth kecil, tapi **harus sudah tahu pool-nya** + kena cap provider (Chainstack 50 akun/stream), dan **tak bisa menangkap pool yang belum ada**.
- **Owner-firehose** (`owner:[program_id]`): otomatis kirim **semua** pool program itu — yang ada **dan yang baru dibuat** (analog `programSubscribe`). Bypass cap 50-akun (1 filter). Bandwidth besar → di sinilah metering (Helius 20 cr/MB) menggigit. **Untuk niche fresh-launchpad, firehose praktis WAJIB** (pool baru tak bisa di-pre-list; deteksi grad via tx-sub ke migration authority).

**Akun yang dilanggan PER POOL (deteksi dislokasi) — terverifikasi dari source:**
| Venue | Akun deteksi | Sizing tambahan |
|---|---|---|
| Orca Whirlpool, Raydium CLMM, Meteora DLMM, Meteora DAMM v2 | **1** (harga di pool account) | +0 langganan (tick/bin = PDA, fetch on-demand via RPC) |
| Raydium CPMM / PumpSwap | **2-3** (2 vault + PoolState) | +0 (closed-form) |
| Raydium AMM v4 | 3 (2 vault + AmmInfo `need_take_pnl`) | messy — prioritas rendah |

**GOTCHA Chainstack:** Chainstack **memblokir** langganan owner ke SPL Token program global (`TokenkegQ...`) + KIN. Konsekuensi: untuk CPMM/AMM v4 yang reserve-nya di vault (milik Token program), kamu **tak bisa owner-firehose vault**; track vault via pubkey eksplisit (setelah pool ditemukan) atau prioritaskan venue yang **harga-nya di pool account** (Whirlpool/CLMM/DLMM/DAMM v2 → firehose bersih).

**Berapa pool dibutuhkan per fase (jawaban sizing):**
- **Bukti mekanisme (Fase 1):** **1 pasang** (1 CPMM + 1 Whirlpool token sama).
- **Profit pertama (MVP, Fase 2):** **~20-50 pasang** targeted (~80-200 akun) — muat di Chainstack ~$98-198/mo (§8).
- **Niche berkelanjutan (Fase 3):** rolling window **~150-300 pasang kandidat** (~50-150 hot), via owner-firehose pool-baru → self-host bila full firehose.
- Pool tipis sepi ~99% waktu → **breadth, bukan depth**: bot arb nyata melanggan banyak *program*, bukan sedikit pool. Universe pool-baru: pump.fun ~10k creations/hari, ~100-270 graduations/hari (2026).

### Reconnect/replay
- Pada disconnect, resubscribe dengan `from_slot` = last processed slot, filter + commitment sama. Bisa terima duplikat → cache harus idempotent. **Karena `write_version` direset/inkomparabel antar sesi:** pada update pertama setelah reconnect, **prefer slot lebih tinggi tanpa syarat**; jangan jatuhkan state baru hanya karena `write_version`-nya lebih kecil dari sesi sebelumnya.

### Two-stage profit gate
1. **Off-chain murah:** graph math + closed-form sizing untuk shortlist.
2. **RPC `simulateTransaction`** (lihat §6 untuk flag yang benar) untuk verifikasi `unitsConsumed` + balance delta sebelum kirim.
3. **On-chain assert** sebagai safety net terakhir (jangan pernah percaya simulasi saja).

### Latency edge & ko-lokasi
Co-locate bot + submission near leaders/RPC (Frankfurt/Amsterdam/Tokyo/NY). Cross-region menambah 30-80ms — fatal untuk pair likuid. Geyser-only bot ~200ms di belakang kompetitor ber-ShredStream + colocated.

---

## 6. Lapisan Eksekusi

### Desain on-chain arbitrage program (native Rust — opinionated)
**Pilih native Rust, bukan Anchor**, untuk hot path: hindari biaya Borsh deser + Anchor account-validation, hasilkan CU lebih rendah & binary kecil. Anchor boleh untuk prototyping/non-hot tooling.

**Pola kanonikal (`buffalojoec/arb-program` sebagai skeleton + pola macro `0xNineteen`):**
```
instruction: TryArbitrage
  1. pre  = read SPL token amount (zero-copy, offset 64 di account layout)
  2. swap CPI leg A  (invoke / invoke_signed)
  3. swap CPI leg B
  4. post = read SPL token amount
  5. require!(post >= pre + min_profit + base_fee + priority + tip,
             ArbitrageError::Unprofitable)   // else Err → runtime reverts ALL
```
- Output tiap swap (`post - pre`) menjadi input swap berikutnya. **Klarifikasi mekanisme chaining (jangan ambigu):** karena kedua swap berjalan di **satu stack-frame instruction yang sama**, carry-nya adalah **balance delta ATA intermediate milik bot** — baca `amount` ATA token-Y setelah leg-1 CPI (sebagai balance delta aktual, lihat Token-2022), feed nilai itu sebagai `amount_in` leg-2. **Tidak perlu PDA `swap_state` yang dipersist** untuk single-ix atomic arb. (Jika `swap_state` PDA memang diinginkan untuk multi-tx, definisikan seeds/layout/rent/re-entrancy secara eksplisit — saat ini undefined di draft.)
- Custom error enum: `NoArbitrage`, `Unprofitable`, `SlippageExceeded`, `InvalidRoute`, `InvalidAccountsList`.
- **Pass pool accounts via `remaining_accounts`** dengan konvensi urutan ketat di sisi client; slice & forward persis akun yang dibutuhkan tiap swap CPI (tiru layout array buffalojoec atau macro per-DEX `basic_amm_swap!`).
- **JANGAN andalkan trik "return NoArbitrage supaya preflight gagal"** sebagai proteksi fee bila kirim dengan `skipPreflight=true` (umum demi kecepatan) — tx gagal akan tetap land dan bakar base fee. **On-chain assert adalah satu-satunya safety net nyata.**

### Spesifikasi yang HARUS dilengkapi (gap dari review — load-bearing)
- **Trust boundary instruction (KRITIS):** pool accounts datang **untrusted** via `remaining_accounts`. Program **wajib** memverifikasi: (a) tiap target swap-CPI adalah **program id DEX yang di-allowlist**; (b) token-account yang dibaca untuk pre/post balance **dimiliki oleh authority/signer bot**, bukan akun arbitrer dari `remaining_accounts` (cegah griefer memberi fake pool / akun yang menyesatkan profit-assert). Definisikan juga **layout instruction-data** eksplisit (mis. `min_profit: u64`, dan jangan percaya cost yang dikirim client — derive sebisanya on-chain).
- **Concurrency / writable-lock self-contention:** banyak peluang di **pool yang sama** = banyak tx yang ambil **writable lock** akun pool yang sama → serialize & saling tabrak (dan di Jito, parallel-auction hanya jalan bila lock tak berpotongan). Wajib **mutex one-inflight-per-writable-account** + dedupe peluang konkuren yang menyentuh pool sama.
- **Budget akun profit-assert:** pembacaan pre/post balance memuat token-account ke tx (hitung ke 256-loaded-accounts + CU deser). Untuk path inventory, ATA ini sudah ter-load oleh swap → marginal ~CU deser saja, tapi **nyatakan eksplisit** di budget, jangan implisit.
- **Blockhash / durable-nonce:** landing loop "rebuild fresh blockhash" menambah RPC round-trip tiap retry. Evaluasi **durable nonce** (system nonce account) untuk resubmit instan tanpa fetch blockhash; tetapkan retry-count/backoff. Double-land aman secara ekonomi (assert revert bila peluang hilang) tapi tetap bakar fee 2×.
- **Invarian inventory (Milestone 1):** hanya eksekusi round-trip yang **kembali ke base asset yang sama** → inventory konservatif. Path apa pun yang menyisakan drift WSOL↔USDC (Fase 3 triangular) butuh kebijakan rebalancing + term cost-of-capital di model unit-economics.
- **Deploy posture program:** Milestone 1 — deploy **upgradeable dengan upgrade authority di Squads multisig** (atau immutable pasca-stabil), publish **verifiable/reproducible build** (`solana-verify`) supaya bytecode on-chain match source, + runbook upgrade.

### CPI: kedalaman & biaya
- CPI depth limit = **5** (→ 9 dengan SIMD-0268). Biaya ~1000 CU/CPI base (→ 946 dgn SIMD-0339). Max CPI account infos 128 (→ 255 dgn SIMD-0339). **Catatan aktivasi:** SIMD-0268 & SIMD-0339 **mengaktif di siklus Agave 3.x** (3.0 mainnet sejak Okt 2025; SIMD-0339 ~Agave 3.1) — kemungkinan **sudah live** pada window eksekusi plan ini. Jangan hardcode nilai pre/post; **baca feature-gate state saat runtime** dan ukur CU/CPI sesungguhnya.
- `invoke` untuk akun yang sudah ditandatangani di tx luar; `invoke_signed` bila program menandatangani sebagai PDA (mis. memiliki vault).

### Hard limits & KOREKSI angka (authoritative)
- **CU:** `MAX_COMPUTE_UNIT_LIMIT = 1,400,000 CU/tx`; default 200,000 CU/instruction (builtins 3,000 CU). Raise via `ComputeBudgetProgram::SetComputeUnitLimit` sampai cap 1.4M.
- **Tx size:** **1,232 bytes** serialized (MTU IPv6 1280 − 48 header). Berlaku untuk **legacy DAN v0** — ALT **tidak** menaikkan cap byte; ALT hanya mengompres key 32-byte → indeks 1-byte.
- **Account-lock limit:** **`MAX_TX_ACCOUNT_LOCKS = 128`** (writable + readonly, dinaikkan dari 64 di v1.14.17). **INI ceiling yang nabrak duluan, BUKAN 256.**
- **Loaded accounts:** v0 tx dapat memuat **maks 256 unique accounts** (indeks u8). Tiap ALT menyimpan ≤256 alamat.
- **Legacy ~35 total akun** (size-bound) → karena itu ALT + v0 wajib untuk multi-pool route. (Bukan "~25-40 writable" — itu konflasi).
- **Signers TIDAK bisa di-load via ALT** — payer/signer harus di static keys (32-byte), memakan budget.
- **Per-writable-account 12M CU/block** → hot pool jadi titik kontensi; fee bersifat LOKAL ke writable account.
- **Whole-block CU cap (terpisah dari per-tx 1.4M):** naik **48M → 60M** (SIMD-0256, ~Epoch 822), dengan SIMD-0286 (100M) & SIMD-0370 (dynamic/no fixed cap, post-Alpenglow) in-flight. Relevan untuk peluang inclusion same-slot saat block penuh; cap per-tx 1.4M tidak terpengaruh.

### Address Lookup Table (ALT) — lifecycle & jebakan
- **256 alamat/tabel** (u8 index); meta 56 bytes; tabel penuh = 56 + 256×32 = 8248 bytes. Append-only (tak bisa hapus per-alamat).
- **Warm-up (KRITIS):** alamat yang baru di-`extend` (termasuk create+extend) **tidak usable sampai slot > slot saat ditambahkan**. **Jangan pernah extend-then-use di slot yang sama** di hot path → v0 key resolution gagal.
- **~30 alamat per extend tx** (legacy 1232-byte limit) → butuh ~9 extend tx untuk isi 256.
- **Close = reclaim rent**, tapi ditolak sampai `deactivation_slot` keluar dari `SlotHashes` (MAX_ENTRIES=512) → **~512 slot (~3.5-5 menit)**. Jalankan "janitor" async, jangan close sinkron.
- **Strategi churn:** (a) STATIC table panjang-umur untuk akun invarian (token/ATA/system/compute-budget program, sysvars, top mints+ATAs, hot pools+vaults+oracles) — di-warm jauh hari; (b) optional per-route table. Pool baru → extend static table (jika <256, terima warm-up 1 slot) atau spin table baru off-path.
- Biaya: create ~0.00128 SOL rent; ~0.00022 SOL/alamat; semua reclaimable saat close.
- Build v0 message via `compileToV0Message(..., [ALT accounts])` agar bucketing writable/readonly benar.

### Profit-assert correctness (load-bearing)
- Bandingkan `post > pre` **dalam input/base asset**, termasuk **semua** fee/tip. Off-by-one atau salah baca akun bisa land tx tidak profitable / exploitable.
- **Token-2022:** profit-check WAJIB dari **balance delta aktual** (baca destination token-account amount sebelum/sesudah tiap leg), **bukan** dari field `amount` di instruction — `TransferFee` memotong fee dari jumlah yang DITERIMA. (Detail di §9.)

### Jito bundles + tips
- Bundle = **≤5 tx**, sequential + atomic, **dalam satu slot**, all-or-nothing. Min tip **1,000 lamports** ke salah satu **8 tip accounts** (resolve via `getTipAccounts` runtime, jangan hardcode).
- **KOREKSI atomicity bundle:** Untuk single-tx arb, atomicity SUDAH dijamin runtime — Jito **tidak** dibutuhkan untuk atomicity. Bundle dibutuhkan hanya untuk: (a) multi-tx (route > 1.4M CU dipecah), (b) revert protection/MEV ordering. Caveat: bila bundle land di **uncled/skipped block** dan tx-nya di-rebroadcast, mereka masuk normal banking stage yang **tidak** menghormati bundle atomicity → **karena itu tx arb tetap WAJIB punya guard profit/slippage sendiri** dan tip ride di tx yang sama.
- **KOREKSI landing:** `sendBundle` mengembalikan `bundle_id` (SHA-256 dari signatures) = **hanya tanda terima, BUKAN jaminan land**. Poll `getInflightBundleStatuses` (window ~5 menit) lalu `getBundleStatuses` (~300 slot). Drop punya banyak penyebab co-dominan: kalah tip auction, kongesti, terlalu dekat akhir slot, simulasi gagal, DAN stale blockhash. **Jangan asumsikan stale blockhash satu-satunya penyebab dominan.**
- Auction tick 50ms, ranked by tip-per-CU. Parallel auction bila account-locks tidak berpotongan.
- Tip sizing dari live feed: REST `https://bundles.jito.wtf/api/v1/bundles/tip_floor` (percentile 25/50/75/95/99 + ema), WS `tip_stream`. Target 50th-75th baseline; skala ke fraksi dari simulated profit. **Cap tip sebagai fraksi profit** supaya kalah-race tidak menggerus modal.
- Endpoint: `https://<region>.mainnet.block-engine.jito.wtf/api/v1/bundles` (ny, amsterdam, dublin, frankfurt, london, slc, singapore, tokyo). Rate limit default **1 req/s/IP/region** → butuh allowlisted UUID (`x-jito-auth`) + regional fan-out.
- `simulateBundle` (Jito-Solana RPC: Triton/Helius/QuickNode lil'JIT) untuk validasi sebelum bayar tip.

### Helius Sender (alternatif landing)
- `https://sender.helius-rpc.com/fast` — dual-route SWQoS + Jito paralel, **0 credits**. WAJIB `skipPreflight=true` + `maxRetries=0` (retry sendiri). Min tip 0.0002 SOL (dual) atau `?swqos_only=true` (0.000005 SOL). Harus sertakan Jito tip transfer + `setComputeUnitPrice`.

### simulateTransaction — flag yang BENAR (KOREKSI)
- `replaceRecentBlockhash:true` dan `sigVerify:true` **mutually exclusive**. Pakai `replaceRecentBlockhash:true` + `sigVerify:false`.
- Return value: `err`, `logs`, `unitsConsumed`, `returnData`, `innerInstructions` (bila diminta), `replacementBlockhash`, `loadedAccountsDataSize`.
- **KOREKSI (draft sebelumnya salah):** `simulateTransaction` **JUGA** mengembalikan `fee`, `preBalances`, `postBalances`, `preTokenBalances`, `postTokenBalances`, dan `loadedAddresses` di value object-nya (terverifikasi di struct `RpcSimulateTransactionResult`). Jadi **tidak perlu** decode SPL amount di offset 64 secara manual — baca `pre/postTokenBalances` langsung untuk profit-check. (Yang benar dari KOREKSI lama: `sigVerify:true` ⟂ `replaceRecentBlockhash:true`, jadi pakai `replaceRecentBlockhash:true` + `sigVerify:false`.)

### Flash loans vs inventory (Milestone 1: INVENTORY)
- **Mulai dengan pre-funded WSOL/USDC inventory.** Runtime sudah memberi revert-if-unprofitable; inventory = tanpa fee, lebih sedikit akun/CU, tanpa constraint top-level-instruction.
- Flash loan hanya relevan bila size opportunity > inventory atau untuk zero idle capital. Biaya/kendala:
  - **Solend/Save:** ~0.30% (`flash_loan_fee_wad=3e15`), host split 20%. Per-reserve.
  - **Kamino KLend** (`KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD`): `flash_loan_fee_sf` per-reserve, **default 0** (bukan sama dengan "disabled"; disabled = `u64::MAX`). Sering 0 di reserve besar.
  - **marginfi v2** (`MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`): **tanpa explicit flash fee**.
- **KOREKSI CPI constraint:** Aturan top-level bersifat protocol-specific. **marginfi v2** mengharuskan `begin_flashloan` & `end_flashloan` keduanya top-level (cek `get_stack_height() == TRANSACTION_LEVEL_STACK_HEIGHT`) → blokir CPI wrapper tunggal. Desain introspection-only (Sanctum Slumlord) mengizinkan Borrow via CPI, hanya CheckRepaid yang top-level. **Untuk plan ini (asumsi marginfi): borrow & repay top-level, client assembles `[begin, ...arb, end]`.** Baca fee reserve on-chain saat startup; gate `(spread - swap_fees - flash_fee - tip - prio) > 0` sebelum sign.

---

## 7. Matematika & Algoritma

### CPMM dua-pool: optimal trade size closed-form
Perlakukan round-trip (beli pool1, jual pool2) sebagai satu composite CPMM. Dengan `g = (1 - fee)` dan reserves:

**Definisi variabel** (round-trip: input base `X` → pool A → token `Y` → pool B → kembali `X`):
`Ra_in, Ra_out` = reserve (`X`, `Y`) di **pool A**; `Rb_in, Rb_out` = reserve (`Y`, `X`) di **pool B**. `g_a = 1 − fee_a`, `g_b = 1 − fee_b`. `delta*` = jumlah `X` optimal yang diinput ke leg pertama.

```
delta* = ( sqrt(g_a · g_b · Ra_in · Ra_out · Rb_in · Rb_out) − Ra_in · Rb_in )
         / ( g_a · Rb_in + g_a · g_b · Ra_out )
```
*(Form umum ini sudah terverifikasi sebagai optimizer sejati — diturunkan ulang secara simbolik.)*

**Reduksi yang BENAR untuk fee 0.3% (`g_a = g_b = 0.997`)** — special-case dari form umum di atas, bukan formula lain:
```
delta* = (997·sqrt(Ra_in·Ra_out·Rb_in·Rb_out) − 1000·Ra_in·Rb_in)
         / (994.009·Ra_out + 997·Rb_in)          // 994.009 = 997²/1000
```
> ⚠️ **Koreksi vs draft lama:** form `(997·√(x1·x2·y1·y2) − 1000·x1·y2)/(997·y1 + 1000·y2)` adalah parametrisasi **berbeda** (donggeunyu / arXiv 2410.10797), variabel `x,y`-nya tak pernah didefinisikan, **dan** ia kehilangan faktor depan `1.003 = 1/(1−fee)` sehingga **selalu meleset ke 0.997× optimum**. Pakai reduksi di atas (konsisten dengan form umum & definisi variabel), atau bila memang mau pakai form donggeunyu, **restore `1.003` + sitasi sumber**. Apa pun pilihannya, tambahkan **contoh numerik** (reserves → `delta*`) sebagai unit test.

**Opportunity ada iff** cross-product reserves × fee factors > no-arb product (≡ `g_a·g_b·Ra_out·Rb_out > Ra_in·Rb_in`). Fungsi profit **strictly concave** dalam size.

### Profit flat near optimum
Bukti empiris (arXiv 2410.10797): searcher mengeksekusi **~54-71% dari optimal size** namun masih menangkap **~81-99% profit** karena kurva strictly-concave & sangat datar di puncak. **Keputusan sizing kita: trade di 90-95% optimum** — *bukan* diturunkan dari angka 60% itu, melainkan pilihan sadar untuk thin pool (mau ambil porsi lebih besar dari profit yang memang kecil) **dengan on-chain assert me-revert tiap miss**. Rasional buffer 90-95% = margin overflow/rounding + opportunity-decay, bukan undershoot latensi. *(Untuk pair likuid/latency-bound, undershoot lebih agresif ke ~60% bisa lebih tepat — pilih per-niche, jangan campur dua angka tanpa alasan.)*

### Swap output & integer math (Raydium-style)
```
amount_in_after_fee = amount_in · (DENOM − fee_num) / DENOM        // mul_div_floor
amount_out = (reserve_out · amount_in_after_fee) / (reserve_in + amount_in_after_fee)
```
- Reserves `u64`; promote ke **u128/u256** sebelum kalikan, lalu turun ke u64. `checked_mul/checked_div`.
- `RoundDirection`: **Floor untuk output, Ceiling untuk required input** — selalu favor pool. Off-chain prediction WAJIB mirror persis rounding ini, kalau tidak realisasi on-chain divergen → revert (bakar CU+fee, no refund).

### CLMM caveats (Raydium CLMM / Orca Whirlpool)
- Tidak ada closed-form global: likuiditas piecewise-constant per tick; `L` melompat (`liquidityNet`) saat lintas initialized tick (cari via tick bitmap). `price = 1.0001^tick`, sqrtPriceX64 Q64.64.
- Sizing: (a) iterasi tick-by-tick (`compute_swap_step`) sampai marginal price dua pool konvergen, atau (b) **ternary/golden-section search** atas input size (profit unimodal), cap ~20-30 iterasi demi CU. Clamp output (bisa overflow u64 utk sqrt_price ekstrem).

### Meteora DLMM (constant-SUM per bin)
- Tiap bin: `P·X + Y = L`, `P = (1 + binStep/10000)^binId`. Swap dalam bin aktif **tanpa slippage** (harga datar) → sizing = isi bin aktif lalu cross ke bin diskret berikutnya. Variable fee (volatility) di atas base fee.

### Triangular / negative-cycle detection
- Graph: node = token, edge = pool, weight `w = −ln(effective_rate_incl_fee)`. **Negative-weight cycle** (Σ −ln < 0 ⇔ produk rate > 1) = arbitrage.
- **Bellman-Ford / SPFA** O(V·E); rekonstruksi cycle via predecessor array.
- **Caveat:** rate adalah marginal (size-0) price → cycle hanya beri **arah**, bukan size. Tiap leg tetap di-size dengan CPMM closed-form / iterative search (price impact mengecilkan profit dengan size). **Jangan pernah trade cycle tanpa re-size.**

### Generalisasi N-pool (untuk fase lanjut)
- Optimal routing lintas N CFMM = convex program (Angeris et al.; `CFMMRouter.jl`). Terlalu berat untuk on-chain 1.4M CU → precompute route/size off-chain, eksekusi + verify on-chain.

### Aturan eksekusi atomic (rangkuman matematis)
Jalankan semua swap dalam satu tx, akhiri dengan `assert(final_out >= start_in + fees + min_profit)`. Gagal → runtime revert ALL state, tapi **CU + fee tetap terbakar (no refund)**. Karena itu off-chain sizing harus mirror integer math + rounding tiap pool secara persis.

---

## 8. Tech Stack & Infrastruktur

### Bahasa: Rust untuk hot path, TS untuk prototyping
- **Rust (wajib untuk produksi kompetitif):** compiled, no GC. V8 GC pauses = jitter latency tak terkendali yang diam-diam kalah same-slot race.
  - Crates: `solana-sdk` (Agave 2.x/4.x), `yellowstone-grpc-client` + `yellowstone-grpc-proto` (rpcpool, v13.x +solana.4.0.0), `jito-sdk-rust`/`jito-rust-json-rpc`, `bytemuck`, `solana_nostd_entrypoint`, `noalloc_allocator!`, `uint`/`spl-math` (U128/U256).
  - On-chain: **native `solana-program`** (hot path), Anchor (`anchor-lang`, `anchor-spl`) untuk tooling/prototyping.
- **TypeScript (prototyping/learning only):** `@solana/web3.js` (≥**1.95.8** — **HINDARI 1.95.6/1.95.7**, CVE-2024-54134) atau `@solana/kit` (web3.js v2), `jito-ts`, `@triton-one/yellowstone-grpc`.

### Detection & landing
- Ingest: **Yellowstone gRPC** (Triton/Helius LaserStream). Landing: **Jito bundles** (primer) + **Helius Sender** (alternatif/fallback).
- Priority fee: Helius `getPriorityFeeEstimate` (`priorityLevel` High/VeryHigh = 75th/95th). `helius-labs/atlas-priority-fee-estimator` (open-source).

### SWQoS (KOREKSI 2025-2026)
- SWQoS mengalokasikan ~**80% kapasitas TPU** ke staked connections, ~20% unstaked, **sebelum** evaluasi priority fee (di QUIC/TPU ingress, upstream dari Banking Stage). Jadi di bawah kongesti, **staked connection adalah prasyarat landing**, bukan sekadar priority fee tinggi.
- **Caveat Agave 4.0/4.1:** split 80/20 kini **congestion-conditional** — throttling staked sender sebagian besar dihilangkan di beban normal, alokasi stake-proportional di-enable kembali hanya saat TPU load ~95%+. Connection slots di-rebalance ~2,000 staked + 2,000 unstaked. Perlakukan 80/20 sebagai perilaku saat kongesti.
- **Catatan:** Jito bundle **bypass** normal TPU → SWQoS terutama relevan untuk path non-bundle/fallback `sendTransaction`.

### Data-source gRPC — ladder biaya (DIPERBARUI; Helius BUKAN default)
> **Helius Business $499/mo BUKAN baseline.** gRPC mainnet Helius terkunci di $499 (Developer $49 = **devnet-only**, jebakan terverifikasi). Naik bertahap sesuai jumlah pool (§5):

| Tahap | Data source | Biaya | Kapan |
|---|---|---|---|
| **0-1 Build + bukti mekanisme** (1 pasang pool) | **Jito ShredStream (GRATIS)** + free WSS (Helius free / dRPC) + JSON-RPC | **~$0-40/mo** | belum perlu gRPC bayar |
| **2 Profit pertama** (~20-50 pasang, targeted) | **Chainstack** Growth $49 + Yellowstone gRPC add-on ($49=2 stream / $149=7) | **~$98-198/mo flat** | gRPC mainnet asli, no per-MB metering |
| **3 Niche / firehose** (~150-300 pasang) | **Self-host** yellowstone/richat node, **atau** Chainstack $449 add-on flat (25 stream) bila tetap targeted | **~$800-1.400** (atau ~$498 Chainstack) | firehose auto-discovery pool baru |
| Kompetitif/MEV-grade (pair likuid) | Triton dedicated / self-host co-located + ShredStream | $2.900+/mo per region | hanya bila race pair likuid |

**Putusan provider:** **Chainstack menang per-dolar** untuk Milestone 1-2 (flat, no metering, ~5× lebih murah dari Helius). **Helius DILEWATI** — kemahalan untuk MVP, kalah self-host saat firehose. Alternatif managed "rasa Helius" lebih murah bila mau ketenangan: **Solana Tracker €200 flat** (failover EU+US) atau **Triton PAYG** ($125 deposit + $0.08/GB, latency terbaik).

**Landing terpisah dari data stream:** **Jito bundle** (primer) + **Helius Sender** (GRATIS, 0 credits — landing saja, tidak butuh langganan gRPC Helius).

**Mitigasi kelemahan Chainstack** (replay buffer hanya ~100 slot / ~1 menit, no 24h backfill): pasangkan **Jito ShredStream (gratis)** + rekonsiliasi via JSON-RPC saat reconnect → resiliensi ~setara Helius dengan biaya jauh lebih kecil.

### Testing substrate (jangan pakai devnet untuk logika berbasis state)
- **LiteSVM** (`litesvm`, + solders TS/Python): unit test tercepat. `set_account`, `add_program`, `send_transaction → TransactionMetadata.compute_units_consumed` (CU per leg), `FailedTransactionMetadata` (assert revert path), `warp_to_slot`, `set_sysvar::<Clock>`.
- **Surfpool/surfnet**: drop-in `solana-test-validator` dengan **lazy mainnet-fork** + cheatcodes (`surfnet_setAccount/setTokenAccount/setMintAccount/cloneProgramAccount/timeTravel/profileTransaction`).
- **`solana-test-validator --clone`** + `solana account --output json-compact`: eager-clone untuk endpoint RPC yang bisa di-hit TS bot.
- **Replay historis deterministik:** snapshot pre-state via Yellowstone/`geyser-grpc-plugin` di slot target; bit-identical via Old Faithful/Jetstreamer (CAR files).
- **Devnet TIDAK merefleksikan pool state mainnet** → wajib mainnet-fork.

---

## 9. Manajemen Risiko & Keamanan

### Biaya failed-tx & kompetisi
- **Mayoritas besar** percobaan atomic arb revert (*estimasi* ~90-99% tergantung bot/program/window — bukan angka tersitasi; ikat ke **revert-rate terinstrumentasi sendiri** begitu live). Spam tetap viable karena tiap revert murah (base + priority; tip tidak terbayar bila di dalam tx yang revert) — *namun* lihat caveat fee-bleed agregat di §10.
- Tip wars menggerus **~50-70% profit** pada pair likuid yang tersaturasi (estimasi bid ceiling, **bukan** rata-rata terukur, **bukan** dari Helius report). Satu data **event-level**: launch TrumpCoin (Mei 2025) total Jito tips ~$8.4M ≈ **14% dari ~$60M profit** — snapshot frenzy, **bukan** rasio tip/gross steady-state bot dominan. **Cap tip sebagai fraksi profit.**
- **Revert rate > 30% = sinyal bug infra**, bukan strategi. Instrumentasi sebagai metrik kesehatan utama.

### Konsentrasi pasar (KOREKSI)
- "Top 3 bot >60% volume" sebenarnya menggambarkan **sandwich**, bukan arbitrage. Pasar arb lebih terfragmentasi/multi-tim. Tetap: pair likuid butuh infra co-located; edge ada di long-tail/pool baru.

### Key management (komponen paling penting & paling sering absen)
- **Hot key WAJIB in-memory** untuk throughput HFT. Remote signer (KMS/HSM/Turnkey/Fireblocks) menambah round-trip puluhan-ratusan ms → fatal untuk auction 50ms/sub-slot. Tidak ada signer single-tier yang sekaligus HFT-cepat dan hardware-isolated.
- **Arsitektur:** thin in-process ed25519 **signing sidecar** memegang **hanya hot key bersaldo kecil**; treasury/program-upgrade/multisig authorities di KMS/HSM/Squads multisig. Bungkus via **Solana Keychain** `SolanaSigner` trait (Memory untuk hot, KMS/Fireblocks untuk treasury) — swap by config.
- **Hot key = vektor kerugian #1.** Pelajaran **supply-chain malware**: @solana/web3.js 1.95.6/7 trojan (CVE-2024-54134, fix ≥1.95.8); repo "Solana-Arbitrage-Bot" yang scrape `PRIVATE_KEY` dari `.env`, bs58-obfuscate URL exfil, POST ke server attacker (SlowMist). **Pin deps + lockfile + integrity hash; jangan pernah jalankan repo bot tak-terpercaya dengan key ber-dana; sandbox semuanya; subscribe GitHub security advisories.**
- **Pelajaran terpisah — opsec/treasury (BUKAN dependency-malware):** breach Step Finance ~$40M (~Jan 2025) berasal dari **device tim eksekutif yang dikompromikan / social engineering**, bukan dependency jahat. Mitigasinya beda: **multisig (Squads) + treasury signing hardware-isolated + device hygiene**, bukan dep-pinning. Jangan konflasikan dua threat model ini.

### Blast-radius caps & kill-switch
- Hard cap saldo hot key; cron/threshold sweeper memindah surplus PnL ke KMS/multisig treasury tiap N menit / saat balance > cap. Treat hot key sebagai expendable, rotate berkala & saat anomali.
- **Kill-switch:** supervisor memegang flag `signing-enabled` yang dicek signer sebelum tiap sign. Auto-off pada lonjakan revert-rate / realized-loss / deviasi saldo; satu perintah manual halt menghentikan semua outflow dalam detik.
- **Sinyal lagging — butuh cap sinkron:** revert-rate & realized-loss baru update setelah status-polling (detik). Pasangkan kill-switch metrik dengan **cap pre-sign SINKRON lokal di signer** (per-interval count + cumulative lamport-out cap dari snapshot saldo, tanpa round-trip) supaya worst-case outflow per window terbatas sebelum metrik menyusul.
- **Signer bukan oracle buta:** sidecar wajib **validasi bentuk tx** terhadap template arb sebelum sign — allowlist program id, destinasi = ATA milik sendiri, cap max-lamport-out — tolak apa pun yang tak match.
- **Threshold konkret (jangan kualitatif):** definisikan angka — mis. revert-rate > X% / N menit, realized-loss > Y SOL/jam, deviasi saldo hot-key > Z — plus alert routing (Telegram/PagerDuty), ekspektasi on-call (atau "unmanned, auto-halt only"), dan **runbook recovery pasca-trip** tertulis.

### MEV / proteksi
- **`jitodontfront`:** sertakan akun read-only non-signer berawalan `jitodontfront` → Block Engine memaksa tx di index 0 atau reject bundle (anti-frontrun). **Hanya berlaku via Jito Block Engine; nol proteksi via RPC biasa; mainnet/testnet only.** Jangan campur routing.
- Atomic arb mengecilkan attack surface: sandwich yang menggeser harga hanya membuat tx revert (rugi fee+tip), drained pool juga revert. Sisa exposure: tx profitable di-copy/front-run (mitigasi: jitodontfront + tip kompetitif) dan fee bleed di priority-fee race (mitigasi: threshold realistis + kill-switch).

### Token-2022 / SPL extensions (bisa diam-diam merusak leg)
- **Filter routing Milestone 1 (HARD-REJECT):** unpack tiap leg mint sebagai `PodStateWithExtensions<PodMint>`; tolak mint dengan `TransferHook`, `NonTransferable`, `DefaultAccountState=frozen`, `MemoTransfer`, `ConfidentialTransfer`, `PermanentDelegate`, **dan `MintCloseAuthority`** (Neodyme: enable reinit-attack & transfer-fee bypass). Izinkan hanya plain SPL + **fee-only Token-2022**. *(Display-only `InterestBearing`/`ScaledUiAmount` aman — raw amount on-chain tak berubah — jadi tidak di-reject.)*
  - **Nuansa `TransferHook`:** mint bisa membawa extension `TransferHook` dengan `program_id = None/null` (mis. PYUSD) dan tetap transfer-safe. Blanket-reject untuk MVP boleh, **tapi dokumentasikan** bahwa ini juga membuang mint null-hook yang sebenarnya aman (tolak hanya bila `TransferHook.program_id = Some(non-null)` kalau mau ambil volume itu).
- **Profit-check dari balance delta aktual** (bukan instruction `amount`) — `TransferFee` potong fee dari jumlah diterima. Bug korektnes paling mungkin.
- Forward vs inverse fee **non-simetris** (beda ≤1 unit, floor division) — jangan tukar. Raydium CP-Swap pakai `get_transfer_fee` (input) & `get_transfer_inverse_fee` (output sizing). CVE nyata (Kora paymaster). Baca fee live per tx (`getEpochFee`); jangan cache lintas epoch.
- **Transfer hooks** (bila nanti diaktifkan): forward extra accounts dari `ExtraAccountMetaList` PDA via `remaining_accounts` di tiap layer; akun ini memakan budget 1232-byte. **KOREKSI seeds:** hanya **dua seed** `[b"extra-account-metas", mint.as_ref()]`, dan PDA diturunkan **di bawah** hook program id — `Pubkey::find_program_address(&[b"extra-account-metas", mint.as_ref()], &hook_program_id)`. `hook_program_id` **BUKAN** seed ketiga; memasukkannya sebagai seed menghasilkan address salah → resolusi hook gagal.

### WSOL dance (wajib tiap tx untuk leg SOL)
`createAssociatedTokenAccountIdempotent` (NATIVE_MINT `So11111111111111111111111111111111111111112`) → `SystemProgram.transfer` lamports → `SyncNative` → ... → `CloseAccount` (unwrap + reclaim rent). ATA program shared Token & Token-2022 — pass token program id yang benar. Assert ATA tidak frozen sebelum dipakai. Lupa `CloseAccount` = rent + WSOL nyangkut.

### Pre-trade pool/mint vetting
- Cek freeze/mint authority (Helius DAS / `getAccountInfo` mint); minta authority renounced untuk token tak dikenal; simulasi SELL via Jupiter quote sebagai heuristik honeypot. **TAPI jangan gate hanya pada Jupiter:** "no quote = honeypot" over-/under-inclusive — pool **fresh launchpad (justru niche kita)** sering belum ter-index Jupiter → filter ini malah membuang niche-nya. Fallback: **readability pool langsung + filter Token-2022 (§9) + simulasi SELL aktual terhadap pool spesifik yang akan diroute** (bukan Jupiter). Perlakukan "quote sukses" sebagai *necessary-not-sufficient*; on-chain assert tetap satu-satunya safety net. Allowlist DEX/pool tervetting (Raydium, Orca, Meteora). On-chain revert melindungi dari reserve hilang mid-route, tapi vetting menghindari buang fee/tip + CU exhaustion di poison route.

### Simulasi ≠ eksekusi
Preflight bisa menunjukkan profit yang lenyap saat landing karena state berubah. On-chain assert (bukan client-sim) **harus** menjadi safety net.

---

## 10. Ekonomi & Ekspektasi Realistis

### Skala & margin
- ~90.4M arb tx sukses, ~$142.8M profit, **average $1.58/arb** (mean berekor panjang; 88.7% dalam SOL; rekor tunggal $3.7M) — angka Helius MEV report, window **trailing-12-bulan (≈2024)**, bukan kalender-2025. Inverse dari model Ethereum (low-frequency/high-margin).
- ~50% volume DEX Solana = arb. MEV revenue Solana ~$720M (Helius Ecosystem H1-2025), melampaui priority fees. *(Catatan: angka $720M **tidak terkonfirmasi** dari primary source spesifik — perlakukan sebagai estimasi, sitasi atau caveat sebelum dipakai untuk keputusan.)*

### Kompetisi & unit economics
- Tip ceiling ~50-70% profit di pair likuid tersaturasi. Pemenang = bare-metal EPYC co-located + Geyser sub-5ms + tip tuning live. Bot di public RPC ~200ms di belakang → kalah ~100% race likuid.
- **Model unit economics sebelum commit modal (per-success):** `net = spread − swap_fees − (flash_fee) − jito_tip − priority_fee − base_fee`. **Tapi model per-success ini OVERSTATE profitabilitas** — biaya dominan nyata adalah priority+base fee yang terbakar di ~90-99% attempt yang revert. Pakai **model probabilistik**: `E[net per opportunity] = p_land·(spread − swap_fees − flash_fee − tip − prio − base) − (1 − p_land)·(prio + base terbakar saat revert) − rent_churn(ALT) − E[rug/honeypot loss di niche]`. Dengan avg $1.58 dan tip leakage 50-70%, pair likuid **hampir selalu negatif** setelah amortisasi loser-burn. **Niche viable: thin/fresh launchpad + memecoin** (kompetisi ~2 orde lebih ringan, throughput dolar lebih kecil, risiko rug/honeypot lebih tinggi). Instrumentasi **burn-rate (lamports/menit di loser)** sebagai metrik kelas-satu di samping revert-rate.

### Capital
- Milestone 1: pre-funded inventory kecil (mis. cukup untuk notional opportunity tipikal di thin pools). Tidak butuh flash loan.
- Hot key bersaldo minutes-to-hours of working capital; sisanya di cold treasury.

### Biaya infra bulanan (target) — DIPERBARUI (lihat ladder §8)
- **Fase 0-1:** **~$0-40/mo** (Jito ShredStream gratis + free WSS; Helius Sender gratis untuk landing).
- **Fase 2:** **~$98-198/mo** (Chainstack Growth + Yellowstone gRPC add-on). **Bukan** Helius $499.
- **Fase 3 (firehose/niche):** ~$498 Chainstack flat **atau** self-host node ~$800-1.400/mo.
- **Fase 4 kompetitif (pair likuid):** $2.900+/mo per region (dedicated/co-located).
- Build estimasi 3-6 bulan (dev berpengalaman) + 5-10 jam/minggu maintenance. **Caveat realisme:** estimasi fase adalah *happy-path* untuk satu dev senior tanpa slip — "3-6 bulan" = Fase 0-3 (Fase 0-3 saja ~11-17 minggu), **mengecualikan** Fase 4 yang ongoing. Fase 2 (landing loop + key mgmt + kill-switch + sweeper + observability) adalah yang **paling berisiko molor** — realistis 4-6 minggu, bukan 3-4. Tambahkan buffer kontingensi eksplisit.

### Putusan jujur
Bangun untuk menguasai arsitektur atomic + menargetkan niche thin-liquidity. **Jangan** harap mengalahkan top searchers di SOL/USDC tanpa co-location. Konfirmasi matematika unit-economics tertutup sebelum belanja infra mahal.

---

## 11. Roadmap Bertahap (INTI DOKUMEN)

> Prinsip lintas-fase: on-chain assert adalah safety net; v0 + ALT sejak hari 1; native Rust untuk hot path; pre-funded inventory dulu; revert-rate sebagai metrik kesehatan utama; uji di mainnet-fork bukan devnet.

### FASE 0 — Riset & Setup (1-2 minggu)
**Goals:** Lingkungan dev siap, sumber kredibel dipelajari, semua program ID terverifikasi on-chain, key security baseline.

**Deliverables konkret:**
- Repo monorepo: `onchain/` (native Rust program), `bot/` (Rust hot path), `infra/` (config, ALT tooling), `tests/` (LiteSVM + Surfpool).
- Toolchain: Rust + `solana-cli` (Agave), Anchor (untuk tooling), LiteSVM, Surfpool. `cargo` workspace.
- Akun: **Jito ShredStream allowlist (gratis)** + free WSS (Helius free / dRPC) untuk Fase 0-1; siapkan **Chainstack Growth + Yellowstone gRPC add-on** untuk Fase 2 (ladder §8). **Helius Sender (gratis)** untuk landing. Jito allowlisted UUID (rate limit bundle), throwaway keypair. *(Helius Business $499 dilewati — lihat §8.)*
- Dokumen config: pin **semua** program ID (verifikasi di Solscan), terutama prop-AMM truncated (HumidiFi/Tessera/GoonFi) ditandai unverified.
- Studi: `buffalojoec/arb-program`, `0xNineteen/rust-macros-arbitrage`, `raydium-cpi-example`, `orca-so/whirlpool-cpi-sample`. **Jangan fork repo "Solana-Arbitrage-Bot" keyword-spam (malware).**

**Checklist Fase 0:**
- [ ] Rust + solana-cli + Anchor + LiteSVM + Surfpool terinstal & jalan
- [ ] Semua program ID di config terverifikasi on-chain; prop-AMM truncated ditandai unverified
- [ ] Jito ShredStream allowlist diperoleh + proxy jalan (gratis); free WSS subscribe test OK; (Fase 2) Chainstack Yellowstone gRPC endpoint siap
- [ ] Jito allowlisted UUID diperoleh; `getTipAccounts` resolved runtime
- [ ] Key security baseline: hot keypair `chmod 600`, tidak di git, deps pinned + lockfile + integrity hash
- [ ] Skeleton `buffalojoec/arb-program` di-build & jalankan test-nya lokal
- [ ] Mainnet-fork (Surfpool) bisa clone Raydium CPMM + Orca Whirlpool pool

---

### FASE 1 — MVP: 2-Pool Atomic Arb di Devnet/Mainnet-Fork (3-5 minggu)
**Goals:** On-chain program `TryArbitrage` (2 swap CPI + profit-assert) yang terbukti revert pada input tidak profitable, divalidasi penuh di mainnet-fork.

**Scope ketat:** Hanya **2-pool CPMM** (Raydium CPMM + Orca Whirlpool), plain SPL + fee-only Token-2022. Closed-form sizing. Pre-funded inventory (no flash loan).

**Sizing pool (§5):** bukti mekanisme = **1 pasang** (1 CPMM + 1 Whirlpool token sama, ~1-3 akun deteksi/pool); siapkan jalan ke **~20-50 pasang targeted** untuk Fase 2. Data source Fase 1 = gratis (ShredStream + free WSS / mainnet-fork) — belum perlu gRPC bayar (§8).

**Deliverables konkret:**
- **On-chain (native Rust):** instruction `TryArbitrage`; balance snapshot via zero-copy (offset 64); 2 swap CPI via `remaining_accounts` (konvensi urutan ketat); terminal `require!(post >= pre + min_profit + costs, Unprofitable)`. Error enum lengkap.
- **Sizing engine (Rust):** CPMM closed-form `delta*` di u128/u256, `mul_div_floor`, mirror rounding Raydium/Orca persis. Trade di 90-95% optimum.
- **Tx builder:** v0 VersionedTransaction + pre-warmed ALT (static table: token/ATA/system/compute-budget programs, sysvars, pool+vault accounts). `SetComputeUnitLimit` (measured CU + 10%), `SetComputeUnitPrice`. WSOL wrap/sync/close helper. Token-2022 extension filter (HARD-REJECT hook/frozen/dll).
- **Detection (Rust):** Yellowstone gRPC subscribe Raydium CPMM + Orca Whirlpool pool accounts (processed); idempotent cache (slot + write_version); decode reserves/sqrtPrice; recompute edge.
- **Pre-flight:** `simulateTransaction` (`replaceRecentBlockhash:true`, `sigVerify:false`); baca `pre/postTokenBalances` (+ `unitsConsumed`, `fee`) dari hasil sim untuk profit-check — tidak perlu decode offset-64 manual.
- **Testing harness:** LiteSVM unit test (assert `Unprofitable` pada no-arb config; assert sukses + delta tepat pada profitable config; ukur CU per leg). Surfpool integration test lawan program Raydium/Orca asli. Patch oracle slot bila perlu (untuk leg non-oracle ini minimal).
- **Differential/property test rounding-mirror (WAJIB, bukan satu contoh):** properti paling rapuh = off-chain Rust mereproduksi integer-math + rounding tiap DEX **bit-exact**. Fuzz `(reserves, fees, amount_in)` rentang lebar, assert `predicted_out == on-chain CPI realized_out` untuk **kedua arah** dan **kedua DEX** (Raydium CP-Swap vs Orca Whirlpool berbeda — implement per-venue), termasuk path fee Token-2022. **Gate Milestone-1 pada test ini**, bukan pada satu happy-path example.

**Checklist Fase 1:**
- [ ] `TryArbitrage` revert dengan `Unprofitable` pada input no-arb (LiteSVM `FailedTransactionMetadata`)
- [ ] `TryArbitrage` sukses + output delta sesuai prediksi off-chain pada input profitable
- [ ] Off-chain predicted profit == on-chain realized profit dibuktikan via **fuzz/property test** (bukan satu contoh), kedua arah & kedua DEX
- [ ] Trust-boundary: program tolak swap-CPI ke program non-allowlist & balance-read dari akun non-bot
- [ ] CU per leg terukur; total tx < 1.4M CU; account locks < 128; tx < 1232 bytes
- [ ] ALT pre-warmed (≥1 slot sebelum dipakai); tidak ada extend-then-use same-slot
- [ ] Token-2022 filter menolak mint hook/frozen/non-transferable/memo/confidential/permanent-delegate **+ mint-close-authority**
- [ ] WSOL wrap→sync→close lengkap; tidak ada ATA nyangkut; assert tidak frozen
- [ ] Detection cache idempotent (dedupe slot+write_version intra-sesi); reconnect prefer slot tertinggi (write_version inkomparabel antar-sesi)
- [ ] simulateTransaction profit-check (decode account data) cocok dengan on-chain
- [ ] Validasi revert dengan input sengaja tidak profitable di mainnet-fork

---

### FASE 2 — Mainnet + Jito (3-4 minggu)
**Goals:** Mendarat profitable di mainnet pada size kecil via Jito bundle, dengan tip di dalam tx atomic, landing loop yang benar, dan key/kill-switch operasional.

**Scope data & venue:** naik ke **Chainstack Yellowstone gRPC** (~$98-198/mo, §8) untuk **~20-50 pasang targeted**. **Tambah PumpSwap AMM** ke set venue (constant-product, mirip CPMM) — wajib untuk menangkap aliran graduation memecoin yang kini ke PumpSwap, bukan Raydium (§4 KOREKSI). Mulai owner-firehose discovery pool-baru (deteksi grad via tx-sub ke migration authority).

**Deliverables konkret:**
- **Jito integration:** build 1-tx bundle (arb + tip transfer ke `getTipAccounts`-resolved, load-balance 8 akun). `jitodontfront` di-stamp. Tip sizing dari `tip_floor`/`tip_stream` (50th-75th baseline, skala ke fraksi simulated profit, cap sebagai fraksi profit). Submit ke regional Block Engine terdekat + allowlisted UUID.
- **`simulateBundle`** (Jito-Solana RPC) sebelum bayar tip.
- **Landing loop ketat:** submit → poll `getInflightBundleStatuses` (5-min) → `getBundleStatuses` (~300 slot). On no-land ~2-3s: **REBUILD fresh blockhash**, resubmit (jangan reuse blockhash). Track penyebab drop (tip kalah / kongesti / stale / sim-fail).
- **Helius Sender fallback** (`skipPreflight=true`, `maxRetries=0`, swqos_only atau dual).
- **Signer sidecar:** in-process ed25519 hot key (saldo kecil) via Solana Keychain `SolanaSigner`; cek `signing-enabled` flag.
- **Kill-switch + sweeper:** supervisor flag (auto-off pada revert-rate spike / loss / deviasi); cron sweeper surplus → cold treasury (KMS/Squads multisig).
- **Observability:** P50/P95 submit latency, confirmation rank, **revert rate**, realized slippage per route, PnL. Alert deviasi.

**Checklist Fase 2:**
- [ ] Data source = Chainstack Yellowstone gRPC (~$98-198/mo), ~20-50 pasang targeted; **Helius $499 dilewati** (§8)
- [ ] **PumpSwap AMM** terintegrasi sebagai venue (tangkap graduation memecoin yang kini ke PumpSwap, bukan Raydium)
- [ ] 1-tx bundle dengan tip di dalam tx atomic (gagal → tip tidak terbayar)
- [ ] Tip accounts di-resolve via `getTipAccounts` runtime (TIDAK hardcode)
- [ ] `jitodontfront` di-stamp; routing eksklusif via Jito (tidak ada fallback non-proteksi diam-diam)
- [ ] Tip sizing dinamis dari tip_floor; tip di-cap sebagai fraksi profit
- [ ] Landing loop: poll status, rebuild fresh blockhash on no-land, jangan reuse blockhash
- [ ] `simulateBundle` pass sebelum kirim
- [ ] Mendarat **profitable** minimal sekali di mainnet (size kecil)
- [ ] Signer sidecar isolasi hot key bersaldo kecil; treasury di KMS/multisig
- [ ] Kill-switch berfungsi (manual halt < detik; auto-trip pada revert-rate/loss)
- [ ] Sweeper memindah surplus ke cold treasury pada threshold
- [ ] Dashboard revert-rate/latency/PnL live; alert deviasi aktif

---

### FASE 3 — Multi-Venue / Triangular / Flash-Loan (4-6 minggu)
**Goals:** Perluas surface ke lebih banyak venue & route 3-leg; opsional flash loan untuk size > inventory.

**Deliverables konkret:**
- **Venue tambahan:** Raydium AMM v4 (V2/tag 16-17, 8 akun bila bisa), Meteora DLMM + DAMM v2, Phoenix (CLOB leg — walk bid/ask ladder). CLMM/DLMM sizing via ternary/golden-section search (profit unimodal) + tick/bin-walk untuk exactness. **Phoenix partial-fill (wajib ditangani):** ladder bisa bergeser antara deteksi & eksekusi → fill parsial bikin input leg-2 lebih kecil dari prediksi. Pakai order **IOC/FOK dengan limit price eksplisit** supaya ladder yang bergerak *cancel* bukan fill adversarial; pastikan profit-assert (balance-delta) revert bersih saat partial; size terhadap depth ladder yang **di-revalidasi**, bukan snapshot lama.
- **Triangular arb:** Bellman-Ford/SPFA negative-cycle di `−log(rate)` graph untuk DISCOVERY/arah; re-size tiap leg dengan impact-aware math. Returns-to-start-token supaya profit checkable dalam input asset.
- **Jupiter sebagai pricer sekunder** (off-chain): two-quote pattern (A→B lalu B→A, concat routePlans, `/swap-instructions` dengan `useSharedAccounts:false`) karena public API blok same-mint (`CIRCULAR_ARBITRAGE_IS_DISABLED`). Tune `maxAccounts` (mulai 64 − N akun wrapper, requote turun sampai < 1232 bytes). **JANGAN CPI Jupiter on-chain untuk 2 leg berat** (inner CPI tak bisa ALT). Jupiter route untuk jangkau prop-AMM (SolFi/HumidiFi/dll) yang tak bisa di-decode. **Jangan pakai Ultra API** (managed, sembunyikan instructions).
- **Flash loan (opsional, asumsi marginfi v2):** client assemble `[ComputeBudget, begin_flashloan, ...arb, end_flashloan]` top-level (bukan CPI wrapper). Baca fee reserve on-chain; prefer Kamino 0-fee reserve / marginfi no-fee atas Solend 0.3%. Gate `(spread − fees − flash_fee − tip − prio) > 0`.
- **ALT churn management:** janitor async (deactivate → tunggu ~512 slot → close untuk reclaim rent); registry pool→{table, index, ready_slot}.

**Checklist Fase 3:**
- [ ] Raydium v4 (V2 8-akun bila ada), Meteora DLMM/DAMM v2, Phoenix terintegrasi & teruji di fork
- [ ] CLMM/DLMM sizing (ternary search) cocok on-chain dalam toleransi
- [ ] Triangular discovery (Bellman-Ford) + per-leg re-sizing; returns-to-start-token
- [ ] Jupiter two-quote pattern untuk pricing (off-chain), `maxAccounts` tuned, tx fit < 1232 bytes
- [ ] Prop-AMM dijangkau via Jupiter route (tidak di-decode custom)
- [ ] (Opsional) Flash loan top-level assembly jalan; fee reserve dibaca on-chain; gate profit-after-fee
- [ ] ALT janitor: deactivate→close reclaim rent; registry pool→table konsisten
- [ ] Account locks tetap < 128 & tx < 1232 bytes pada route multi-hop (ALT)

---

### FASE 4 — Optimization (ongoing)
**Goals:** Latency edge kompetitif, CU minimization, co-location, tip strategy adaptif.

**Deliverables konkret:**
- **CU minimization:** native Rust agresif — `bytemuck` zero-copy, `#[inline(always)]`, `solana_nostd_entrypoint`, `noalloc_allocator!`, bitwise instruction-data parse. Benchmark CU per leg; set `SetComputeUnitLimit` presisi.
- **ShredStream:** allowlisted keypair, gRPC decode, co-locate near Jito region (Frankfurt/NY/Amsterdam). Decode swap tx (tx-intent) untuk reaksi same-slot.
- **Co-location:** dedicated/bare-metal EPYC near high-stake validator; staked RPC. Regional fan-out submission (2 region).
- **Adaptive tip:** model tip vs acceptance-rate live; rolling block telemetry.
- **BAM readiness:** abstraksi layer tip/ordering di balik interface supaya bisa migrasi dari Block Engine auction ke **BAM** (Block Assembly Marketplace, TEE scheduler + sequencing plugins) saat mencapai majority stake.
- **Backtest corpus:** 10-50 arb historis (menang & kalah) via Geyser snapshot / Old Faithful; regression gate realized vs predicted profit sebelum deploy modal.

**Checklist Fase 4:**
- [ ] CU per leg di-optimasi & limit presisi (no overpay)
- [ ] ShredStream aktif (allowlisted) + co-located near Jito region
- [ ] Staked RPC / SWQoS untuk path fallback non-bundle
- [ ] Regional submission fan-out; allowlisted UUID > 1 req/s
- [ ] Adaptive tip model berdasarkan acceptance-rate
- [ ] Interface tip/ordering siap-migrasi ke BAM
- [ ] Golden-replay corpus sebagai regression gate (predicted == realized dalam toleransi)

---

## 12. Checklist Milestone & Definition of Done

### Definition of Done — Milestone 1 (Atomic Arb)
- [ ] On-chain native Rust `TryArbitrage`: 2 swap CPI + terminal profit-assert; revert pada `Err(Unprofitable)`, runtime rollback ALL state.
- [ ] Terbukti revert pada input tidak profitable (LiteSVM + Surfpool + mainnet small-size), tanpa pergerakan token bersih.
- [ ] Off-chain predicted profit == on-chain realized profit (integer math + rounding mirror, u128/u256), **dibuktikan via fuzz/property test per-venue, dua arah** — bukan satu contoh.
- [ ] **Trust boundary on-chain:** swap-CPI hanya ke program id allowlist; balance-read hanya dari token-account milik bot (tahan griefer via `remaining_accounts`).
- [ ] v0 VersionedTransaction + pre-warmed ALT; account locks < 128; tx < 1232 bytes; CU < 1.4M; `SetComputeUnitLimit` measured+10%.
- [ ] WSOL wrap/sync/close lengkap; Token-2022 extension filter aktif (incl. mint-close-authority); profit-check dari balance delta aktual.
- [ ] Mendarat profitable ≥1 kali di mainnet via Jito 1-tx bundle dengan tip di dalam tx atomic.
- [ ] Landing loop: status polling (`getInflightBundleStatuses`→`getBundleStatuses`), rebuild fresh blockhash on no-land.
- [ ] Key security: hot key sidecar bersaldo kecil + treasury KMS/multisig; kill-switch + sweeper operasional.
- [ ] Observability: revert-rate, latency P50/P95, PnL ter-dashboard; alert deviasi.
- [ ] Unit economics terdokumentasi: `net = spread − swap_fees − tip − prio − base > 0` pada target niche.

### Gerbang kualitas lintas-fase
- [ ] Tidak ada dependensi tak-terpercaya dijalankan dengan key ber-dana (audit + sandbox).
- [ ] Semua program ID terverifikasi on-chain & pinned di config.
- [ ] Tidak ada extend-then-use ALT same-slot di hot path.
- [ ] On-chain assert (bukan client-sim) sebagai safety net final; valid bahkan saat `skipPreflight=true`.
- [ ] Program dideploy upgradeable (authority di Squads multisig) atau immutable; **verifiable build** (`solana-verify`) dipublish.
- [ ] Signer enforce **cap pre-sign sinkron** + validasi bentuk tx; kill-switch metrik punya threshold numerik + runbook.

---

## 13. Referensi & Sumber

### Repo studi (kredibel — bukan malware)
- `buffalojoec/arb-program` — native Rust atomic arb, pola `NoArbitrage` revert, zero-copy, ALT (skeleton utama).
- `0xNineteen/blog.md` (rust-macros-arbitrage) — pola profit-revert + macro per-DEX CPI.
- `raydium-io/raydium-cpi-example`, `orca-so/whirlpool-cpi-sample`, `MeteoraAg/cpi-examples` / `damm-v2` / `dlmm-sdk` — official CPI scaffolds.
- `Ellipsis-Labs/phoenix-v1` + phoenix-sdk — CLOB leg.
- `raydium-io/raydium-amm` (`program/src/instruction.rs`, `processor.rs`, `math.rs`) — account counts & integer math authoritative.
- `raydium-io/raydium-clmm` (`swap_math.rs`, `full_math.rs`) — CLMM tick math.
- `jordan-public/flash-loan-unlimited-solana`, `2501babe/adobe`, `igneous-labs/slumlord` — flash-loan patterns (fase lanjut).
- `rpcpool/yellowstone-grpc`, `jito-labs/shredstream-proxy`, `jito-foundation/geyser-grpc-plugin` — streaming.
- `bcc-research/CFMMRouter.jl`, `jacksonConrad/better-simple-arbitrage`, `flashbots/simple-blind-arbitrage` — sizing math.

### Dokumentasi inti
- Solana: `solana.com/docs/core/transactions`, `/fees`, `/fees/compute-budget`, `/cpi`; `docs.anza.xyz/proposals/versioned-transactions`; `mina86.com/2025/solana-tx-size-limits`.
- Jito: `docs.jito.wtf/lowlatencytxnsend`, `/lowlatencytxnfeed`; `jito-foundation.gitbook.io` (on-chain addresses, bundles); `bam.dev`.
- Yellowstone/Triton: `blog.triton.one/complete-guide-to-solana-streaming-and-yellowstone-grpc`; `docs.triton.one`.
- Helius: `helius.dev/blog/solana-mev-report`, `/solana-mev-an-introduction`, `/optimizing-solana-programs`, `/solanas-proprietary-amm-revolution`, `/stake-weighted-quality-of-service`; `helius.dev/docs/sending-transactions/sender`, `/priority-fee-api`.
- SWQoS: `solana.com/developers/guides/advanced/stake-weighted-qos`; `blog.triton.one/evolution-of-solanas-stake-weighted-quality-of-service`.
- Jupiter: `developers.jup.ag/docs/api/swap-api/swap-instructions`, `/swap/v1/requote-with-lower-max-accounts`, `/swap/build-swap-transaction`; `github.com/jup-ag/jupiter-swap-api/issues/76`.
- Token-2022: `neodyme.io/en/blog/token-2022`; `blog.offside.io/p/token-2022-security-best-practices-part-2`; `solana.com/developers/guides/token-extensions/transfer-hook`.
- Flash loans: `deepwiki.com/solendprotocol/.../flash-loans`; `docs.marginfi.com/mfi-v2` & `/faqs`; `github.com/Kamino-Finance/klend`; `sanctum.so/blog/introducing-slumlord`.
- ALT: `docs.anza.xyz/proposals/versioned-transactions`; `solana.com/developers/guides/advanced/lookup-tables`; `docs.solanamevbot.com/.../address-lookup-table`; `github.com/solana-labs/solana/issues/27241`.
- Testing: `docs.rs/litesvm`; `docs.surfpool.run`; `helius.dev/blog/surfpool`; `solana.com/docs/rpc/http/simulatetransaction`.
- Math: arXiv `2402.06731`, `2305.14604`, `2410.10797` (60%-optimum), `2103.02228`, `2406.16573`, `2204.05238` (Angeris); `rareskills.io/post/uniswap-v3-concentrated-liquidity`.
- Security: `github.com/.../GHSA-jcxm-7wvp-g6p5` (web3.js CVE); `slowmist.medium.com` (malicious bot analysis); `solana.com/docs/tools/keychain`; `solana.com/developers/guides/advanced/mev-protection`.
- Ekonomi: `helius.dev/blog/solana-mev-report`; `academy.extropy.io/.../mev-crosschain-analysis-2025`; `explorer.jito.wtf/arbitrage-overview`.

> **Peringatan keamanan akhir:** Cluster repo "Solana-Arbitrage-Bot" (ChangeYourself0613, OnlyForward0613, senior106, znjqolf, AV1080p, WSOL12, keidev-sol, kelvin-1013, dll.) adalah wallet-draining malware terdokumentasi (SlowMist). **JANGAN PERNAH** jalankan dengan key ber-dana. Studi hanya sumber yang telah diaudit di atas, dan jalankan segalanya di sandbox dengan throwaway keypair.
