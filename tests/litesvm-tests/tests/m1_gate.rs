//! M1-GATE: the rounding-mirror differential. Runs the real `TryArbitrage` SBF program over
//! two controlled constant-product pools (the `swap-harness` deployed at an allowlisted DEX
//! id) and asserts the ON-CHAIN realized round-trip output equals the OFF-CHAIN
//! `arb_math::RoundTrip::realized_out` prediction, bit-for-bit. Also proves the program
//! reverts on an unprofitable round-trip and on a non-allowlisted swap-CPI target
//! (onchain-9/10, sizing-9, testing-5 collapsed per implementation-plan section 9.3).
//!
//! Needs the two build-sbf artifacts via env: `ARB_PROGRAM_SO`, `SWAP_HARNESS_SO` (set by the
//! WSL runner). Self-skips if unset so a host `cargo test` without build-sbf stays green.

use arb_math::{CpmmReserves, RoundTrip};
use arb_program::instruction::{LegDescriptor, TryArbitrageData};
use arb_types::{DexKind, SwapDir};
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const FEE_NUM: u64 = 25;
const FEE_DEN: u64 = 10_000;

fn so(var: &str) -> Option<Vec<u8>> {
    std::fs::read(std::env::var(var).ok()?).ok()
}

/// The harness binary is deployed AT this allowlisted id so the arb-program trust boundary
/// accepts the CPI. (Pubkey rebuilt from bytes to dodge any cross-crate type duplication.)
fn allowlisted_dex() -> Pubkey {
    Pubkey::new_from_array(arb_config::WAVE1_DEX_ALLOWLIST[0].to_bytes())
}

/// An SPL-token-layout account (owner field at 32, amount at 64), owned ON-CHAIN by
/// `onchain_owner` so the harness program may edit the balance directly.
fn token_account(owner_field: &Pubkey, amount: u64, onchain_owner: &Pubkey) -> Account {
    let mut data = vec![0u8; 165];
    data[32..64].copy_from_slice(owner_field.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1; // AccountState::Initialized
    Account {
        lamports: 1_000_000,
        data,
        owner: *onchain_owner,
        executable: false,
        rent_epoch: 0,
    }
}

struct Gate {
    send_ok: bool,
    realized_final: Option<u64>,
    predicted_final: Option<u64>,
    logs: String,
}

/// Build a fresh SVM, set up base+intermediate ATAs and the two pools, run one round-trip
/// (base to Y via pool A AtoB, Y to base via pool B AtoB), and return realized vs predicted.
#[allow(clippy::too_many_arguments)]
fn run_gate(
    arb_so: &[u8],
    harness_so: &[u8],
    dex_id: Pubkey, // program id the harness is deployed at (allowlisted for success)
    leg_dex_account: Pubkey, // id passed to the program as each leg dex program
    pool_a: (u64, u64), // (reserve_a, reserve_b); leg A is AtoB (base in = reserve_a)
    pool_b: (u64, u64), // leg B is AtoB (Y in = reserve_a)
    base_funding: u64,
    delta_in: u64,
    min_profit: u64,
) -> Gate {
    let mut svm = LiteSVM::new();
    let arb_id = Pubkey::new_unique();
    svm.add_program(arb_id, arb_so).unwrap();
    svm.add_program(dex_id, harness_so).unwrap();

    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 1_000_000_000).unwrap();

    // Token-layout accounts, all owned on-chain by the harness so it can move balances.
    let base_ata = Pubkey::new_unique();
    let inter_ata = Pubkey::new_unique();
    let pa_in = Pubkey::new_unique();
    let pa_out = Pubkey::new_unique();
    let pb_in = Pubkey::new_unique();
    let pb_out = Pubkey::new_unique();

    svm.set_account(
        base_ata,
        token_account(&authority.pubkey(), base_funding, &dex_id),
    )
    .unwrap();
    svm.set_account(inter_ata, token_account(&authority.pubkey(), 0, &dex_id))
        .unwrap();
    svm.set_account(pa_in, token_account(&authority.pubkey(), pool_a.0, &dex_id))
        .unwrap();
    svm.set_account(
        pa_out,
        token_account(&authority.pubkey(), pool_a.1, &dex_id),
    )
    .unwrap();
    svm.set_account(pb_in, token_account(&authority.pubkey(), pool_b.0, &dex_id))
        .unwrap();
    svm.set_account(
        pb_out,
        token_account(&authority.pubkey(), pool_b.1, &dex_id),
    )
    .unwrap();

    let data = TryArbitrageData {
        min_profit,
        leg_a: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: SwapDir::AtoB,
            account_count: 4,
            amount_in: delta_in,
            min_out: 0,
        },
        leg_b: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: SwapDir::AtoB,
            account_count: 4,
            amount_in: 0, // use the measured intermediate delta from leg A
            min_out: 0,
        },
    }
    .pack();

    let metas = vec![
        AccountMeta::new(authority.pubkey(), true), // [0] authority (signer, payer)
        AccountMeta::new(base_ata, false),          // [1] base_ata
        AccountMeta::new(inter_ata, false),         // [2] intermediate_ata
        AccountMeta::new_readonly(leg_dex_account, false), // [3] leg A dex program
        AccountMeta::new(base_ata, false),          // [4] leg A user_source
        AccountMeta::new(inter_ata, false),         // [5] leg A user_dest
        AccountMeta::new(pa_in, false),             // [6] leg A pool_src
        AccountMeta::new(pa_out, false),            // [7] leg A pool_dst
        AccountMeta::new_readonly(leg_dex_account, false), // [8] leg B dex program
        AccountMeta::new(inter_ata, false),         // [9] leg B user_source
        AccountMeta::new(base_ata, false),          // [10] leg B user_dest
        AccountMeta::new(pb_in, false),             // [11] leg B pool_src
        AccountMeta::new(pb_out, false),            // [12] leg B pool_dst
    ];

    let ix = Instruction {
        program_id: arb_id,
        accounts: metas,
        data: data.to_vec(),
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        svm.latest_blockhash(),
    );
    let res = svm.send_transaction(tx);

    let pool_a_r = CpmmReserves::new(pool_a.0, pool_a.1, FEE_NUM, FEE_DEN);
    let pool_b_r = CpmmReserves::new(pool_b.0, pool_b.1, FEE_NUM, FEE_DEN);
    let rt = RoundTrip::new(pool_a_r, SwapDir::AtoB, pool_b_r, SwapDir::AtoB);
    let predicted_final = rt.realized_out(delta_in);

    match res {
        Ok(_) => {
            let acct = svm.get_account(&base_ata).unwrap();
            let post_base = u64::from_le_bytes(acct.data[64..72].try_into().unwrap());
            // post_base = base_funding - delta_in + final => final = post_base + delta_in - base_funding
            let realized_final = post_base + delta_in - base_funding;
            Gate {
                send_ok: true,
                realized_final: Some(realized_final),
                predicted_final,
                logs: String::new(),
            }
        }
        Err(meta) => Gate {
            send_ok: false,
            realized_final: None,
            predicted_final,
            logs: format!("{:?}", meta.err),
        },
    }
}

fn artifacts() -> Option<(Vec<u8>, Vec<u8>)> {
    Some((so("ARB_PROGRAM_SO")?, so("SWAP_HARNESS_SO")?))
}

#[test]
fn m1_gate_differential_roundtrip_matches_offchain() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP m1_gate_differential: set ARB_PROGRAM_SO + SWAP_HARNESS_SO");
        return;
    };
    let dex = allowlisted_dex();
    // Profitable two-pool edge from the arb_math example (a=1M/2M, b=2M/1.1M), small sizes.
    for delta in [1_000u64, 5_000, 25_000] {
        let g = run_gate(
            &arb,
            &harness,
            dex,
            dex,
            (1_000_000, 2_000_000),
            (2_000_000, 1_100_000),
            1_000_000,
            delta,
            0,
        );
        assert!(
            g.send_ok,
            "delta={delta} expected success, got revert: {}",
            g.logs
        );
        assert_eq!(
            g.realized_final, g.predicted_final,
            "M1-GATE DRIFT at delta={delta}: on-chain realized {:?} != off-chain predicted {:?}",
            g.realized_final, g.predicted_final
        );
        // sanity: the chosen sizes are genuinely profitable round-trips
        assert!(
            g.realized_final.unwrap() > delta,
            "delta={delta} should be profitable"
        );
    }
}

#[test]
fn m1_gate_reverts_when_unprofitable() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP m1_gate_reverts_when_unprofitable: set env");
        return;
    };
    let dex = allowlisted_dex();
    // min_profit far above any achievable net -> terminal assert must revert ALL state.
    let g = run_gate(
        &arb,
        &harness,
        dex,
        dex,
        (1_000_000, 2_000_000),
        (2_000_000, 1_100_000),
        1_000_000,
        5_000,
        10_000_000,
    );
    assert!(!g.send_ok, "huge min_profit must revert, but tx succeeded");
}

#[test]
fn reverts_on_non_allowlisted_dex() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP reverts_on_non_allowlisted_dex: set env");
        return;
    };
    let allowed = allowlisted_dex();
    // Harness deployed at the allowlisted id, but the leg dex-program account passed to the
    // program is a DIFFERENT, non-allowlisted id -> verify_swap_program must reject pre-CPI.
    let rogue = Pubkey::new_unique();
    let g = run_gate(
        &arb,
        &harness,
        allowed,
        rogue,
        (1_000_000, 2_000_000),
        (2_000_000, 1_100_000),
        1_000_000,
        5_000,
        0,
    );
    assert!(!g.send_ok, "non-allowlisted dex program must be rejected");
}
