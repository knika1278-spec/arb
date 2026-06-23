//! onchain-6 / onchain-11 / testing-8 — the REAL-VENUE M1-GATE differential in **LiteSVM**.
//!
//! Drives the **real** Raydium CP-Swap program (`CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`,
//! dumped from mainnet) over the **real** SOL/USDC-class pool `7e6L4d…` (real PoolState +
//! AmmConfig + observation + vaults + mints, snapshotted from mainnet) and asserts the program's
//! realized swap output equals our off-chain `arb_math::cpmm` quote **bit-for-bit**, in BOTH
//! directions. This is the residual the LiteSVM CP harness (`m1_gate.rs`) could not prove: there
//! the venue math was our stand-in `swap-harness`; here it is the **real Raydium program**.
//!
//! Why LiteSVM and not Surfpool: the Surfpool fork reverts `InvalidAccountData` inside the real
//! Anchor program at ~4391 CU (a zero-copy/spl deserialize failure over surfpool's forked-account
//! representation — a substrate limitation, NOT an off-chain-math defect; see the stranded
//! `surfpool_real_venue.rs`). LiteSVM serves the snapshotted account bytes verbatim and aligned, so
//! the real program executes and the differential closes here.
//!
//! Fixtures (program `.so` + account `.bin` + `manifest.txt`) are produced by
//! `scripts/dump_raydium_cpmm.py` against the keyed Chainstack node and live in
//! `$REAL_VENUE_FIXTURES`. The test self-skips when that dir is absent so a plain host/CI
//! `cargo test` (no fixtures) stays green. The differential is reserve-snapshot-reproducible: the
//! predicted side is quoted over the SAME reserves the loaded vault bytes carry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arb_math::CpmmReserves;
use arb_types::SwapDir;
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const RAYDIUM_CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const AUTH_SEED: &[u8] = b"vault_and_lp_mint_auth_seed";
/// Anchor `sha256("global:swap_base_input")[..8]`.
const SWAP_BASE_INPUT_DISCRIMINATOR: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];

// PoolState offsets (absolute; past the 8-byte anchor disc) — confirmed vs raydium-cp-swap.
const OFF_PROTOCOL_FEES_0: usize = 341;
const OFF_PROTOCOL_FEES_1: usize = 349;
const OFF_FUND_FEES_0: usize = 357;
const OFF_FUND_FEES_1: usize = 365;
const OFF_AMMCONFIG_TRADE_FEE_RATE: usize = 12;
/// PoolState.open_time (u64 unix seconds) — the real program rejects a swap whose block timestamp
/// is not strictly past it (`ErrorCode::NotApproved` = 6000). LiteSVM's clock starts at genesis, so
/// we warp the Clock sysvar past this before swapping.
const OFF_OPEN_TIME: usize = 373;
const RAYDIUM_CPMM_FEE_DENOMINATOR: u64 = 1_000_000;
const AMOUNT_OFFSET: usize = 64;

fn fixtures_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("REAL_VENUE_FIXTURES").ok()?);
    if dir.join("manifest.txt").exists() {
        Some(dir)
    } else {
        None
    }
}

struct Entry {
    pubkey: Pubkey,
    owner: Pubkey,
    lamports: u64,
}

/// Parse `manifest.txt` (`role pubkey owner lamports`) into a role -> Entry map.
fn read_manifest(dir: &Path) -> HashMap<String, Entry> {
    let text = std::fs::read_to_string(dir.join("manifest.txt")).expect("manifest.txt");
    let mut m = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 4 {
            continue;
        }
        m.insert(
            f[0].to_string(),
            Entry {
                pubkey: f[1].parse().expect("pubkey"),
                owner: f[2].parse().expect("owner"),
                lamports: f[3].parse().expect("lamports"),
            },
        );
    }
    m
}

fn read_u64(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

/// SPL token-account bytes: mint@0, owner@32, amount@64, AccountState::Initialized@108.
fn token_account_bytes(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1;
    d
}

fn token_amount(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key)
        .map(|a| read_u64(&a.data, AMOUNT_OFFSET))
        .unwrap_or(0)
}

/// One direction's differential. `dir` AtoB = token0->token1 (input_vault=vault0); BtoA reversed.
/// `divisor` sizes the trade as `reserve_in / divisor` (so the output is always nonzero regardless
/// of the pool's price/decimals). Returns (realized_out, predicted_out) or None on revert (logged).
fn run_real_swap(dir: SwapDir, divisor: u64) -> Option<(u64, u64)> {
    let dir_dir = fixtures_dir()?;
    let man = read_manifest(&dir_dir);
    let load = |p: &Pubkey| std::fs::read(dir_dir.join(format!("{p}.bin"))).expect("fixture .bin");

    let mut svm = LiteSVM::new();
    let cpmm: Pubkey = RAYDIUM_CPMM.parse().unwrap();
    let spl: Pubkey = SPL_TOKEN.parse().unwrap();
    svm.add_program(
        cpmm,
        &std::fs::read(dir_dir.join("cpmm.so")).expect("cpmm.so"),
    )
    .unwrap();
    svm.add_program(
        spl,
        &std::fs::read(dir_dir.join("spl_token.so")).expect("spl_token.so"),
    )
    .unwrap();

    // Materialize every snapshotted pool account verbatim at its real pubkey.
    for role in [
        "pool",
        "amm_config",
        "observation",
        "vault0",
        "vault1",
        "mint0",
        "mint1",
    ] {
        let e = &man[role];
        svm.set_account(
            e.pubkey,
            Account {
                lamports: e.lamports.max(1_000_000),
                data: load(&e.pubkey),
                owner: e.owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    }

    let pool = man["pool"].pubkey;
    let amm_config = man["amm_config"].pubkey;
    let observation = man["observation"].pubkey;
    let vault0 = man["vault0"].pubkey;
    let vault1 = man["vault1"].pubkey;
    let mint0 = man["mint0"].pubkey;
    let mint1 = man["mint1"].pubkey;

    let pool_data = load(&pool);
    let cfg_data = load(&amm_config);
    let trade_fee_rate = read_u64(&cfg_data, OFF_AMMCONFIG_TRADE_FEE_RATE);

    // Warp the Clock past the pool's open_time so the real program permits the swap.
    let open_time = read_u64(&pool_data, OFF_OPEN_TIME);
    let clock = Clock {
        slot: 350_000_000,
        epoch_start_timestamp: open_time as i64,
        epoch: 810,
        leader_schedule_epoch: 810,
        unix_timestamp: (open_time + 3600) as i64,
    };
    svm.set_sysvar(&clock);
    let protocol_0 = read_u64(&pool_data, OFF_PROTOCOL_FEES_0);
    let protocol_1 = read_u64(&pool_data, OFF_PROTOCOL_FEES_1);
    let fund_0 = read_u64(&pool_data, OFF_FUND_FEES_0);
    let fund_1 = read_u64(&pool_data, OFF_FUND_FEES_1);

    // Curve reserves exactly as Raydium derives them: vault.amount - protocol_fees - fund_fees.
    let v0 = read_u64(&load(&vault0), AMOUNT_OFFSET);
    let v1 = read_u64(&load(&vault1), AMOUNT_OFFSET);
    let r0 = v0 - protocol_0 - fund_0;
    let r1 = v1 - protocol_1 - fund_1;

    // Orient input/output vault+mint by direction.
    let (in_vault, out_vault, in_mint, out_mint, reserve_in) = match dir {
        SwapDir::AtoB => (vault0, vault1, mint0, mint1, r0),
        SwapDir::BtoA => (vault1, vault0, mint1, mint0, r1),
    };
    let amount_in = (reserve_in / divisor).max(1);
    let reserves = CpmmReserves::new(r0, r1, trade_fee_rate, RAYDIUM_CPMM_FEE_DENOMINATOR);
    let predicted = reserves.quote_out(dir, amount_in).expect("off-chain quote");
    eprintln!("[{dir:?}] open_time={open_time} r0={r0} r1={r1} v0={v0} v1={v1} fee={trade_fee_rate}/1e6 amount_in={amount_in} predicted_out={predicted}");

    // A fresh user with a funded input ATA (huge, so the leg never underflows) + empty output ATA.
    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 1_000_000_000).unwrap();
    let user_in = Pubkey::new_unique();
    let user_out = Pubkey::new_unique();
    let funding = amount_in.saturating_mul(4).max(1_000_000_000);
    svm.set_account(
        user_in,
        Account {
            lamports: 2_039_280,
            data: token_account_bytes(&in_mint, &user.pubkey(), funding),
            owner: spl,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    svm.set_account(
        user_out,
        Account {
            lamports: 2_039_280,
            data: token_account_bytes(&out_mint, &user.pubkey(), 0),
            owner: spl,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();

    let (authority, _bump) = Pubkey::find_program_address(&[AUTH_SEED], &cpmm);

    let mut data = SWAP_BASE_INPUT_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // minimum_amount_out

    let metas = vec![
        AccountMeta::new(user.pubkey(), true),       // 0 payer (signer)
        AccountMeta::new_readonly(authority, false), // 1 authority PDA
        AccountMeta::new_readonly(amm_config, false), // 2 amm_config
        AccountMeta::new(pool, false),               // 3 pool_state
        AccountMeta::new(user_in, false),            // 4 input_token_account
        AccountMeta::new(user_out, false),           // 5 output_token_account
        AccountMeta::new(in_vault, false),           // 6 input_vault
        AccountMeta::new(out_vault, false),          // 7 output_vault
        AccountMeta::new_readonly(spl, false),       // 8 input_token_program
        AccountMeta::new_readonly(spl, false),       // 9 output_token_program
        AccountMeta::new_readonly(in_mint, false),   // 10 input_token_mint
        AccountMeta::new_readonly(out_mint, false),  // 11 output_token_mint
        AccountMeta::new(observation, false),        // 12 observation_state
    ];
    let ix = Instruction {
        program_id: cpmm,
        accounts: metas,
        data,
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&user.pubkey()),
        &[&user],
        svm.latest_blockhash(),
    );

    match svm.send_transaction(tx) {
        Ok(_) => {
            let realized = token_amount(&svm, &user_out);
            eprintln!(
                "real Raydium {dir:?} amount_in={amount_in}: realized={realized} predicted={predicted} (r0={r0} r1={r1} fee={trade_fee_rate}/1e6)"
            );
            Some((realized, predicted))
        }
        Err(e) => {
            eprintln!(
                "real Raydium {dir:?} amount_in={amount_in} REVERTED: {:?}",
                e.err
            );
            None
        }
    }
}

#[test]
fn real_raydium_cpmm_differential_both_directions() {
    if fixtures_dir().is_none() {
        eprintln!("SKIP real_raydium_cpmm_differential: set REAL_VENUE_FIXTURES (run scripts/dump_raydium_cpmm.py)");
        return;
    }
    let mut checked = 0u32;
    for dir in [SwapDir::AtoB, SwapDir::BtoA] {
        // amount_in = reserve_in / divisor: ~0.01%, 0.1%, 1% of the input reserve.
        for divisor in [10_000u64, 1_000, 100] {
            let (realized, predicted) = run_real_swap(dir, divisor).unwrap_or_else(|| {
                panic!("real Raydium swap {dir:?} divisor={divisor} reverted (see log)")
            });
            assert_eq!(
                realized, predicted,
                "REAL-VENUE DRIFT {dir:?} divisor={divisor}: on-chain {realized} != off-chain {predicted}"
            );
            assert!(predicted > 0, "{dir:?} divisor={divisor} produced 0 out");
            checked += 1;
        }
    }
    eprintln!("REAL-VENUE M1-GATE GREEN (LiteSVM): real Raydium CP-Swap == arb_math::cpmm, bit-exact, {checked} cases both directions");
}
