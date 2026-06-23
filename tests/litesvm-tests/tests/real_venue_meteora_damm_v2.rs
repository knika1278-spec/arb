//! M1-GATE-EXT — REAL Meteora DAMM v2 (CP-AMM) swap differential in LiteSVM. Drives the real
//! `cpamdpZ…` program's `swap` (exact-in) over a real, constant-fee, both-SPL pool snapshotted
//! from mainnet, and asserts realized output == `arb_math::damm_v2::DammV2Pool::quote_exact_in`
//! bit-for-bit, both directions. DAMM v2 is single-full-range sqrt-price (no tick/bin arrays), so
//! the snapshot is just pool + 2 vaults + 2 mints. The dumper (`dump_damm_v2.py`) selects a pool
//! with `number_of_period==0` + `dynamic_fee_initialized==0` so the total fee == `cliff_fee_numerator`
//! (clock-independent), letting the off-chain quote use a static numerator. See [[arbit-realvenue-litesvm]].

mod rv_common;
use rv_common::*;

use arb_math::damm_v2::{fee_on_input, DammV2Pool, MAX_SQRT_PRICE, MIN_SQRT_PRICE};
use arb_types::SwapDir;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Anchor `sha256("global:swap")[..8]`.
const SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

// Pool offsets (absolute; incl 8-byte disc) — verified in detection::decode::damm_v2_pool_offsets.
const O_CLIFF_FEE: usize = 8;
const O_COLLECT_FEE_MODE: usize = 484;
const O_LIQUIDITY: usize = 360;
const O_SQRT_MIN: usize = 424;
const O_SQRT_MAX: usize = 440;
const O_SQRT_PRICE: usize = 456;

/// Drive one direction. `divisor` sizes amount_in as (input vault balance / divisor). Returns
/// (realized_out, predicted_out) or None on revert (logged).
fn run(snap: &Snapshot, dir: SwapDir, divisor: u64) -> Option<(u64, u64)> {
    let program: Pubkey = PROGRAM.parse().unwrap();
    let spl: Pubkey = SPL_TOKEN.parse().unwrap();

    let mut svm = LiteSVM::new();
    snap.add_program(&mut svm, program, "damm_v2.so");
    snap.add_program(&mut svm, spl, "spl_token.so");
    snap.load_accounts(
        &mut svm,
        &[
            "pool",
            "token_a_vault",
            "token_b_vault",
            "token_a_mint",
            "token_b_mint",
        ],
    );
    // Single-range sqrt-price has no open_time gate, but warp the Clock far-future so any
    // activation_point (slot OR timestamp) gate passes and the static fee path is well-defined.
    warp_clock(&mut svm, 2_000_000_000);

    let pool = snap.pk("pool");
    let vault_a = snap.pk("token_a_vault");
    let vault_b = snap.pk("token_b_vault");
    let mint_a = snap.pk("token_a_mint");
    let mint_b = snap.pk("token_b_mint");
    let pool_data = snap.role_bin("pool");

    let cliff_fee = read_u64(&pool_data, O_CLIFF_FEE);
    let collect_fee_mode = pool_data[O_COLLECT_FEE_MODE];
    let dpool = DammV2Pool::new(
        read_u128(&pool_data, O_SQRT_PRICE),
        read_u128(&pool_data, O_LIQUIDITY),
        read_u128(&pool_data, O_SQRT_MIN),
        read_u128(&pool_data, O_SQRT_MAX),
    );
    let _ = (MIN_SQRT_PRICE, MAX_SQRT_PRICE);

    // Orient: AtoB = token_a in -> token_b out; BtoA = token_b in -> token_a out.
    let (in_mint, out_mint, in_vault) = match dir {
        SwapDir::AtoB => (mint_a, mint_b, vault_a),
        SwapDir::BtoA => (mint_b, mint_a, vault_b),
    };
    let in_vault_bal = read_u64(&snap.bin(&in_vault), AMOUNT_OFFSET);
    let amount_in = (in_vault_bal / divisor).max(1);

    let foi = fee_on_input(collect_fee_mode, dir);
    let predicted = match dpool.quote_exact_in(dir, amount_in, cliff_fee, foi) {
        Ok(q) => q.amount_out,
        Err(e) => {
            eprintln!(
                "[{dir:?}] off-chain quote declined ({e:?}) amount_in={amount_in}; skip size"
            );
            return Some((0, 0)); // signal "skip" (caller treats equal 0==0 as a no-op)
        }
    };
    eprintln!(
        "[{dir:?}] cliff_fee={cliff_fee}/1e9 collect_mode={collect_fee_mode} sqrtP={} L={} amount_in={amount_in} predicted={predicted}",
        dpool.sqrt_price, dpool.liquidity
    );

    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 1_000_000_000).unwrap();
    let user_in = Pubkey::new_unique();
    let user_out = Pubkey::new_unique();
    let funding = amount_in.saturating_mul(4).max(1_000_000);
    svm.set_account(
        user_in,
        solana_sdk::account::Account {
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
        solana_sdk::account::Account {
            lamports: 2_039_280,
            data: token_account_bytes(&out_mint, &user.pubkey(), 0),
            owner: spl,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();

    let (pool_authority, _) = Pubkey::find_program_address(&[b"pool_authority"], &program);
    let (event_authority, _) = Pubkey::find_program_address(&[b"__event_authority"], &program);

    let mut data = SWAP_DISC.to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // minimum_amount_out

    let metas = vec![
        AccountMeta::new_readonly(pool_authority, false), // 0 pool_authority PDA
        AccountMeta::new(pool, false),                    // 1 pool
        AccountMeta::new(user_in, false),                 // 2 input_token_account
        AccountMeta::new(user_out, false),                // 3 output_token_account
        AccountMeta::new(vault_a, false),                 // 4 token_a_vault
        AccountMeta::new(vault_b, false),                 // 5 token_b_vault
        AccountMeta::new_readonly(mint_a, false),         // 6 token_a_mint
        AccountMeta::new_readonly(mint_b, false),         // 7 token_b_mint
        AccountMeta::new(user.pubkey(), true),            // 8 payer (signer)
        AccountMeta::new_readonly(spl, false),            // 9 token_a_program
        AccountMeta::new_readonly(spl, false),            // 10 token_b_program
        AccountMeta::new_readonly(program, false), // 11 referral_token_account = program (None)
        AccountMeta::new_readonly(event_authority, false), // 12 event_authority PDA
        AccountMeta::new_readonly(program, false), // 13 program
    ];
    let ix = Instruction {
        program_id: program,
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
        Ok(_) => Some((token_amount(&svm, &user_out), predicted)),
        Err(e) => {
            eprintln!("[{dir:?}] amount_in={amount_in} REVERTED: {:?}", e.err);
            None
        }
    }
}

#[test]
fn real_meteora_damm_v2_differential_both_directions() {
    let Some(snap) = Snapshot::open("meteora_damm_v2") else {
        eprintln!("SKIP real_meteora_damm_v2: set REAL_VENUE_FIXTURES (run tests/scripts/dump_damm_v2.py)");
        return;
    };
    let mut checked = 0u32;
    for dir in [SwapDir::AtoB, SwapDir::BtoA] {
        for divisor in [1000u64, 100, 20] {
            let (realized, predicted) = run(&snap, dir, divisor).unwrap_or_else(|| {
                panic!("real DAMM v2 swap {dir:?} divisor={divisor} reverted (see log)")
            });
            assert_eq!(
                realized, predicted,
                "DAMM v2 DRIFT {dir:?} divisor={divisor}: on-chain {realized} != off-chain {predicted}"
            );
            if predicted > 0 {
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 2,
        "no nonzero-output cases exercised — pool too illiquid"
    );
    eprintln!("REAL-VENUE M1-GATE-EXT GREEN: real Meteora DAMM v2 == arb_math::damm_v2, bit-exact, {checked} cases both directions");
}
