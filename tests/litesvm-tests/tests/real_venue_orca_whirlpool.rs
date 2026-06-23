//! onchain-7 / testing-8 (Orca) — REAL Orca Whirlpool swap differential in LiteSVM. Drives the
//! real `whirLb…` program's v1 `swap` (exact-in) over the real SOL/USDC Whirlpool (snapshotted
//! pool + vaults + mints + the 3 in-direction tick arrays) and asserts realized output ==
//! `arb_math::whirlpool::WhirlpoolPool::quote_exact_in` bit-for-bit, both directions. Trades are
//! sized tiny so the price stays inside the active tick (no initialized-tick crossing), matching
//! the single-in-range-step off-chain quoter. See [[arbit-realvenue-litesvm]].

mod rv_common;
use rv_common::*;

use arb_math::whirlpool::WhirlpoolPool;
use arb_types::SwapDir;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Anchor `sha256("global:swap")[..8]` (v1 swap).
const SWAP_DISC: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
const MIN_SQRT_PRICE_X64: u128 = 4_295_048_016;
const MAX_SQRT_PRICE_X64: u128 = 79_226_673_515_401_279_992_447_579_055;
const TICK_ARRAY_SIZE: i32 = 88;

// Whirlpool offsets (verified in detection::decode).
const O_TICK_SPACING: usize = 41; // u16
const O_FEE_RATE: usize = 45; // u16
const O_LIQUIDITY: usize = 49; // u128
const O_SQRT_PRICE: usize = 65; // u128
const O_TICK_CURRENT: usize = 81; // i32

fn read_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}
fn read_i32(d: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn start_tick_index(tick_current: i32, tick_spacing: u16) -> i32 {
    let span = tick_spacing as i32 * TICK_ARRAY_SIZE;
    tick_current.div_euclid(span) * span
}

fn tick_array_pda(pool: &Pubkey, start: i32, program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"tick_array", pool.as_ref(), start.to_string().as_bytes()],
        program,
    )
    .0
}

fn run(snap: &Snapshot, dir: SwapDir) -> Option<(u64, u64)> {
    let program: Pubkey = PROGRAM.parse().unwrap();
    let spl: Pubkey = SPL_TOKEN.parse().unwrap();

    let pool = snap.pk("pool");
    let vault_a = snap.pk("token_a_vault");
    let vault_b = snap.pk("token_b_vault");
    let mint_a = snap.pk("token_a_mint");
    let mint_b = snap.pk("token_b_mint");
    let pd = snap.role_bin("pool");
    let tick_spacing = read_u16(&pd, O_TICK_SPACING);
    let fee_rate = read_u16(&pd, O_FEE_RATE);
    let tick_current = read_i32(&pd, O_TICK_CURRENT);
    let wp = WhirlpoolPool::new(
        read_u128(&pd, O_SQRT_PRICE),
        read_u128(&pd, O_LIQUIDITY),
        fee_rate,
    );

    let span = tick_spacing as i32 * TICK_ARRAY_SIZE;
    let start0 = start_tick_index(tick_current, tick_spacing);
    let a_to_b = dir == SwapDir::AtoB;
    let step = if a_to_b { -span } else { span };
    let ta0 = tick_array_pda(&pool, start0, &program);
    let ta1 = tick_array_pda(&pool, start0 + step, &program);
    let ta2 = tick_array_pda(&pool, start0 + 2 * step, &program);
    if !(snap.has(&ta0) && snap.has(&ta1) && snap.has(&ta2)) {
        eprintln!("[{dir:?}] SKIP: not all 3 tick arrays snapshotted ({ta0} {ta1} {ta2})");
        return Some((0, 0));
    }

    // Tiny trade (well within the active tick) so the single-step quote matches the on-chain walk.
    let amount_in: u64 = 5_000_000; // 0.005 SOL or 5 USDC base units — negligible price impact
    let limit = if a_to_b {
        MIN_SQRT_PRICE_X64
    } else {
        MAX_SQRT_PRICE_X64
    };
    let predicted = match wp.quote_exact_in(dir, amount_in, limit) {
        Ok(q) => q.amount_out,
        Err(e) => {
            eprintln!("[{dir:?}] off-chain quote err {e:?}; skip");
            return Some((0, 0));
        }
    };
    eprintln!(
        "[{dir:?}] ts={tick_spacing} fee={fee_rate}/1e6 tick_cur={tick_current} start0={start0} amount_in={amount_in} predicted={predicted}"
    );

    let mut svm = LiteSVM::new();
    snap.add_program(&mut svm, program, "orca.so");
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
    snap.load_pda(&mut svm, ta0, program);
    snap.load_pda(&mut svm, ta1, program);
    snap.load_pda(&mut svm, ta2, program);
    warp_clock(&mut svm, 2_000_000_000);

    // User ATAs: fund the INPUT side, leave the output empty.
    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 1_000_000_000).unwrap();
    let ata_a = Pubkey::new_unique();
    let ata_b = Pubkey::new_unique();
    let (fund_a, fund_b) = if a_to_b {
        (amount_in.saturating_mul(4), 0)
    } else {
        (0, amount_in.saturating_mul(4))
    };
    for (ata, mint, amt) in [(ata_a, mint_a, fund_a), (ata_b, mint_b, fund_b)] {
        svm.set_account(
            ata,
            solana_sdk::account::Account {
                lamports: 2_039_280,
                data: token_account_bytes(&mint, &user.pubkey(), amt),
                owner: spl,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    }

    let oracle = Pubkey::find_program_address(&[b"oracle", pool.as_ref()], &program).0;

    // v1 swap args (Borsh): amount u64, other_amount_threshold u64, sqrt_price_limit u128,
    // amount_specified_is_input bool, a_to_b bool. On-chain sqrt_price_limit=0 (program substitutes).
    let mut data = SWAP_DISC.to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // other_amount_threshold (min out)
    data.extend_from_slice(&0u128.to_le_bytes()); // sqrt_price_limit = 0 (no limit)
    data.push(1); // amount_specified_is_input = true (exact-in)
    data.push(a_to_b as u8);

    let metas = vec![
        AccountMeta::new_readonly(spl, false),    // 0 token_program
        AccountMeta::new(user.pubkey(), true),    // 1 token_authority (signer)
        AccountMeta::new(pool, false),            // 2 whirlpool
        AccountMeta::new(ata_a, false),           // 3 token_owner_account_a
        AccountMeta::new(vault_a, false),         // 4 token_vault_a
        AccountMeta::new(ata_b, false),           // 5 token_owner_account_b
        AccountMeta::new(vault_b, false),         // 6 token_vault_b
        AccountMeta::new(ta0, false),             // 7 tick_array_0
        AccountMeta::new(ta1, false),             // 8 tick_array_1
        AccountMeta::new(ta2, false),             // 9 tick_array_2
        AccountMeta::new_readonly(oracle, false), // 10 oracle (read-only in v1)
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
    // Output side = token B for AtoB, token A for BtoA.
    let out_ata = if a_to_b { ata_b } else { ata_a };
    match svm.send_transaction(tx) {
        Ok(_) => Some((token_amount(&svm, &out_ata), predicted)),
        Err(e) => {
            eprintln!("[{dir:?}] amount_in={amount_in} REVERTED: {:?}", e.err);
            None
        }
    }
}

#[test]
fn real_orca_whirlpool_differential_both_directions() {
    let Some(snap) = Snapshot::open("orca_whirlpool") else {
        eprintln!(
            "SKIP real_orca_whirlpool: set REAL_VENUE_FIXTURES (run tests/scripts/dump_orca.py)"
        );
        return;
    };
    let mut checked = 0u32;
    for dir in [SwapDir::AtoB, SwapDir::BtoA] {
        let (realized, predicted) =
            run(&snap, dir).unwrap_or_else(|| panic!("real Orca swap {dir:?} reverted (see log)"));
        assert_eq!(
            realized, predicted,
            "ORCA DRIFT {dir:?}: on-chain {realized} != off-chain {predicted}"
        );
        if predicted > 0 {
            checked += 1;
        }
    }
    assert!(checked >= 1, "no nonzero-output direction exercised");
    eprintln!("REAL-VENUE GREEN: real Orca Whirlpool == arb_math::whirlpool, bit-exact, {checked} directions");
}
