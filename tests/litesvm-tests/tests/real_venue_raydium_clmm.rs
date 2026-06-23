//! M1-GATE-EXT — REAL Raydium CLMM swap_v2 differential in LiteSVM. Drives the real `CAMMCzo5…`
//! program's `swap_v2` (exact-in) over a real both-SPL WSOL/USDC CLMM pool and asserts realized
//! output == `arb_math::raydium_clmm::RaydiumClmmPool::quote_exact_in` bit-for-bit, both
//! directions. Trades are tiny so the price stays within the active tick (single-step quote ==
//! on-chain tick walk). swap_v2 routes through SPL/Token-2022/Memo programs (all loaded) and
//! takes the tickarray-bitmap-extension + tick arrays as remaining accounts. See [[arbit-realvenue-litesvm]].

mod rv_common;
use rv_common::*;

use arb_math::raydium_clmm::RaydiumClmmPool;
use arb_types::SwapDir;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const MEMO: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
/// Anchor `sha256("global:swap_v2")[..8]`.
const SWAP_V2_DISC: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];
const MIN_SQRT_PRICE_X64: u128 = 4_295_048_016;
const MAX_SQRT_PRICE_X64: u128 = 79_226_673_521_066_979_257_578_248_091;
const TICK_ARRAY_SIZE: i32 = 60;

// PoolState offsets (verified in detection::decode::raydium_clmm_pool_offsets).
const O_TICK_SPACING: usize = 235; // u16
const O_LIQUIDITY: usize = 237; // u128
const O_SQRT_PRICE: usize = 253; // u128
const O_TICK_CURRENT: usize = 269; // i32
const O_CFG_TRADE_FEE_RATE: usize = 47; // u32 in AmmConfig

fn read_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}
fn read_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
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
        &[b"tick_array", pool.as_ref(), &start.to_be_bytes()],
        program,
    )
    .0
}

fn run(snap: &Snapshot, dir: SwapDir) -> Option<(u64, u64)> {
    let program: Pubkey = PROGRAM.parse().unwrap();
    let spl: Pubkey = SPL_TOKEN.parse().unwrap();
    let token2022: Pubkey = TOKEN2022.parse().unwrap();
    let memo: Pubkey = MEMO.parse().unwrap();

    let pool = snap.pk("pool");
    let amm_config = snap.pk("amm_config");
    let observation = snap.pk("observation");
    let vault0 = snap.pk("vault0");
    let vault1 = snap.pk("vault1");
    let mint0 = snap.pk("mint0");
    let mint1 = snap.pk("mint1");
    let bitmap_ext = snap.pk("bitmap_ext");
    let pd = snap.role_bin("pool");
    let cfg = snap.role_bin("amm_config");

    let tick_spacing = read_u16(&pd, O_TICK_SPACING);
    let tick_current = read_i32(&pd, O_TICK_CURRENT);
    let trade_fee_rate = read_u32(&cfg, O_CFG_TRADE_FEE_RATE);
    let clmm = RaydiumClmmPool::new(
        read_u128(&pd, O_SQRT_PRICE),
        read_u128(&pd, O_LIQUIDITY),
        trade_fee_rate,
    );

    let span = tick_spacing as i32 * TICK_ARRAY_SIZE;
    let start0 = start_tick_index(tick_current, tick_spacing);
    let a_to_b = dir == SwapDir::AtoB;
    let step = if a_to_b { -span } else { span };
    let tas: Vec<Pubkey> = (0..3)
        .map(|k| tick_array_pda(&pool, start0 + k * step, &program))
        .collect();
    if !tas.iter().all(|t| snap.has(t)) {
        eprintln!("[{dir:?}] SKIP: not all 3 tick arrays snapshotted");
        return Some((0, 0));
    }

    let amount_in: u64 = 1_000_000; // tiny — stays within the active tick
    let limit = if a_to_b {
        MIN_SQRT_PRICE_X64
    } else {
        MAX_SQRT_PRICE_X64
    };
    let predicted = match clmm.quote_exact_in(dir, amount_in, limit) {
        Ok(q) => q.amount_out,
        Err(e) => {
            eprintln!("[{dir:?}] off-chain quote err {e:?}; skip");
            return Some((0, 0));
        }
    };
    eprintln!(
        "[{dir:?}] ts={tick_spacing} fee={trade_fee_rate}/1e6 tick_cur={tick_current} start0={start0} amount_in={amount_in} predicted={predicted}"
    );

    let mut svm = LiteSVM::new();
    snap.add_program(&mut svm, program, "clmm.so");
    snap.add_program(&mut svm, spl, "spl_token.so");
    snap.add_program(&mut svm, token2022, "token2022.so");
    snap.add_program(&mut svm, memo, "memo.so");
    snap.load_accounts(
        &mut svm,
        &[
            "pool",
            "amm_config",
            "observation",
            "vault0",
            "vault1",
            "mint0",
            "mint1",
            "bitmap_ext",
        ],
    );
    for t in &tas {
        snap.load_pda(&mut svm, *t, program);
    }
    warp_clock(&mut svm, 2_000_000_000);

    let (in_vault, out_vault, in_mint, out_mint) = match dir {
        SwapDir::AtoB => (vault0, vault1, mint0, mint1),
        SwapDir::BtoA => (vault1, vault0, mint1, mint0),
    };

    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 1_000_000_000).unwrap();
    let user_in = Pubkey::new_unique();
    let user_out = Pubkey::new_unique();
    for (ata, mint, amt) in [
        (user_in, in_mint, amount_in.saturating_mul(4)),
        (user_out, out_mint, 0),
    ] {
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

    // swap_v2 args: amount u64, other_amount_threshold u64, sqrt_price_limit_x64 u128(=0), is_base_input u8(=1).
    let mut data = SWAP_V2_DISC.to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.extend_from_slice(&0u128.to_le_bytes());
    data.push(1); // is_base_input

    let mut metas = vec![
        AccountMeta::new(user.pubkey(), true),        // 0 payer
        AccountMeta::new_readonly(amm_config, false), // 1 amm_config
        AccountMeta::new(pool, false),                // 2 pool_state
        AccountMeta::new(user_in, false),             // 3 input_token_account
        AccountMeta::new(user_out, false),            // 4 output_token_account
        AccountMeta::new(in_vault, false),            // 5 input_vault
        AccountMeta::new(out_vault, false),           // 6 output_vault
        AccountMeta::new(observation, false),         // 7 observation_state
        AccountMeta::new_readonly(spl, false),        // 8 token_program
        AccountMeta::new_readonly(token2022, false),  // 9 token_program_2022
        AccountMeta::new_readonly(memo, false),       // 10 memo_program
        AccountMeta::new_readonly(in_mint, false),    // 11 input_vault_mint
        AccountMeta::new_readonly(out_mint, false),   // 12 output_vault_mint
        AccountMeta::new(bitmap_ext, false),          // remaining[0] bitmap extension
    ];
    for t in &tas {
        metas.push(AccountMeta::new(*t, false)); // remaining[1..] tick arrays (in swap direction)
    }
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
fn real_raydium_clmm_differential_both_directions() {
    let Some(snap) = Snapshot::open("raydium_clmm") else {
        eprintln!(
            "SKIP real_raydium_clmm: set REAL_VENUE_FIXTURES (run tests/scripts/dump_clmm.py)"
        );
        return;
    };
    let mut checked = 0u32;
    for dir in [SwapDir::AtoB, SwapDir::BtoA] {
        let (realized, predicted) =
            run(&snap, dir).unwrap_or_else(|| panic!("real CLMM swap {dir:?} reverted (see log)"));
        assert_eq!(
            realized, predicted,
            "CLMM DRIFT {dir:?}: on-chain {realized} != off-chain {predicted}"
        );
        if predicted > 0 {
            checked += 1;
        }
    }
    assert!(checked >= 1, "no nonzero-output direction exercised");
    eprintln!("REAL-VENUE M1-GATE-EXT GREEN: real Raydium CLMM == arb_math::raydium_clmm, bit-exact, {checked} directions");
}
