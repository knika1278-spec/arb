//! testing-11 — the N-leg (triangle) differential. Runs the real `TryArbitrage` SBF program's
//! N-leg path (`onchain-20`, instruction tag 1) over a THREE-pool cycle
//! `base → t1 → t2 → base` (the `swap-harness` CP program deployed at an allowlisted DEX id) and
//! asserts the ON-CHAIN realized round-trip output equals the OFF-CHAIN prediction
//! (`arb_math::cycle::cycle_net_out`, the chained `CpmmReserves::quote_out`), bit-for-bit. Also
//! proves the N-leg terminal assert reverts on an unprofitable cycle.
//!
//! This exercises the novel on-chain N-leg control flow (snapshot → CPI A → measured delta →
//! CPI B → measured delta → CPI C → terminal base assert) with the SAME bit-identical CP harness
//! the 2-leg M1-GATE uses, so it needs no new venue harness and no Fase-2.5 allowlist change.
//!
//! Needs the two build-sbf artifacts via env: `ARB_PROGRAM_SO`, `SWAP_HARNESS_SO`. Self-skips if
//! unset so a host `cargo test` without build-sbf stays green.

use arb_math::cycle::{cycle_net_out, CycleLeg};
use arb_math::{CpmmReserves, CpmmVenue};
use arb_program::instruction::{LegDescriptor, TryArbitrageNData};
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

fn artifacts() -> Option<(Vec<u8>, Vec<u8>)> {
    Some((so("ARB_PROGRAM_SO")?, so("SWAP_HARNESS_SO")?))
}

/// The harness is deployed AT this allowlisted id so the arb-program trust boundary accepts the
/// CPI (rebuilt from bytes to dodge any cross-crate type duplication).
fn allowlisted_dex() -> Pubkey {
    Pubkey::new_from_array(arb_config::WAVE1_DEX_ALLOWLIST[0].to_bytes())
}

/// An SPL-token-layout account (owner @32, amount @64), owned ON-CHAIN by `onchain_owner` so the
/// harness may edit the balance directly.
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

struct Tri {
    send_ok: bool,
    realized_final: Option<u64>,
    predicted_final: Option<u64>,
    logs: String,
}

/// Build a fresh SVM, set up the 3 cycle ATAs + 3 CP pools, run one `base → t1 → t2 → base`
/// round-trip through the N-leg path, and return realized vs predicted final base out.
#[allow(clippy::too_many_arguments)]
fn run_triangle(
    arb_so: &[u8],
    harness_so: &[u8],
    pools: [(u64, u64); 3], // each (reserve_in, reserve_out) for that leg's AtoB swap
    base_funding: u64,
    delta_in: u64,
    min_profit: u64,
) -> Tri {
    let mut svm = LiteSVM::new();
    let arb_id = Pubkey::new_unique();
    let dex_id = allowlisted_dex();
    svm.add_program(arb_id, arb_so).unwrap();
    svm.add_program(dex_id, harness_so).unwrap();

    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 1_000_000_000).unwrap();

    // 3 cycle ATAs (base, t1, t2), all owned on-chain by the harness so it can move balances.
    let ata: [Pubkey; 3] = [
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ];
    svm.set_account(
        ata[0],
        token_account(&authority.pubkey(), base_funding, &dex_id),
    )
    .unwrap();
    svm.set_account(ata[1], token_account(&authority.pubkey(), 0, &dex_id))
        .unwrap();
    svm.set_account(ata[2], token_account(&authority.pubkey(), 0, &dex_id))
        .unwrap();

    // Per-pool reserve accounts (src = input-side reserve, dst = output-side reserve).
    let mut pool_src = [Pubkey::default(); 3];
    let mut pool_dst = [Pubkey::default(); 3];
    for i in 0..3 {
        pool_src[i] = Pubkey::new_unique();
        pool_dst[i] = Pubkey::new_unique();
        svm.set_account(
            pool_src[i],
            token_account(&authority.pubkey(), pools[i].0, &dex_id),
        )
        .unwrap();
        svm.set_account(
            pool_dst[i],
            token_account(&authority.pubkey(), pools[i].1, &dex_id),
        )
        .unwrap();
    }

    // N-leg instruction (tag 1): 3 legs, all RaydiumCpmm/AtoB; leg 0 takes the explicit input,
    // legs 1/2 chain the measured delta (amount_in = 0).
    let leg = |amount_in: u64| LegDescriptor {
        dex: DexKind::RaydiumCpmm,
        dir: SwapDir::AtoB,
        account_count: 4,
        amount_in,
        min_out: 0,
    };
    let nd = TryArbitrageNData::from_legs(min_profit, &[leg(delta_in), leg(0), leg(0)]).unwrap();
    let (buf, used) = nd.pack();

    // Account list: authority + 3 cycle ATAs + per leg [dex_program][src,dst,pool_src,pool_dst].
    // Leg i swaps ata[i] -> ata[(i+1) % 3]; the harness reads [user_src, user_dst, pool_src,
    // pool_dst].
    let mut metas = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new(ata[0], false),
        AccountMeta::new(ata[1], false),
        AccountMeta::new(ata[2], false),
    ];
    for i in 0..3 {
        metas.push(AccountMeta::new_readonly(dex_id, false)); // leg i dex program (allowlisted)
        metas.push(AccountMeta::new(ata[i], false)); // user_source
        metas.push(AccountMeta::new(ata[(i + 1) % 3], false)); // user_dest
        metas.push(AccountMeta::new(pool_src[i], false));
        metas.push(AccountMeta::new(pool_dst[i], false));
    }

    let ix = Instruction {
        program_id: arb_id,
        accounts: metas,
        data: buf[..used].to_vec(),
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        svm.latest_blockhash(),
    );
    let res = svm.send_transaction(tx);

    // Off-chain prediction: chain the three CP legs (the sizing-15 composition).
    let venues: Vec<CpmmVenue> = pools
        .iter()
        .map(|&(a, b)| {
            CpmmVenue::new(
                DexKind::RaydiumCpmm,
                CpmmReserves::new(a, b, FEE_NUM, FEE_DEN),
            )
        })
        .collect();
    let legs: Vec<CycleLeg> = venues
        .iter()
        .map(|v| CycleLeg::new(v, SwapDir::AtoB))
        .collect();
    let predicted_final = cycle_net_out(&legs, delta_in);

    match res {
        Ok(_) => {
            let acct = svm.get_account(&ata[0]).unwrap();
            let post_base = u64::from_le_bytes(acct.data[64..72].try_into().unwrap());
            // post_base = base_funding - delta_in + final => final = post_base + delta_in - base_funding
            let realized_final = post_base + delta_in - base_funding;
            Tri {
                send_ok: true,
                realized_final: Some(realized_final),
                predicted_final,
                logs: String::new(),
            }
        }
        Err(meta) => Tri {
            send_ok: false,
            realized_final: None,
            predicted_final,
            logs: format!("{:?}", meta.err),
        },
    }
}

/// A dislocated triangle whose loop product exceeds 1: base→t1 (1M/2M), t1→t2 (1M/2M),
/// t2→base (1M/4.4M). Small sizes are profitable round-trips.
const TRIANGLE: [(u64, u64); 3] = [
    (1_000_000, 2_000_000),
    (1_000_000, 2_000_000),
    (1_000_000, 4_400_000),
];

#[test]
fn triangle_gate_differential_matches_offchain() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP triangle_gate_differential: set ARB_PROGRAM_SO + SWAP_HARNESS_SO");
        return;
    };
    for delta in [1_000u64, 5_000, 25_000] {
        let g = run_triangle(&arb, &harness, TRIANGLE, 1_000_000, delta, 0);
        assert!(
            g.send_ok,
            "delta={delta} expected success, got revert: {}",
            g.logs
        );
        assert_eq!(
            g.realized_final, g.predicted_final,
            "TRIANGLE-GATE DRIFT at delta={delta}: on-chain {:?} != off-chain {:?}",
            g.realized_final, g.predicted_final
        );
        assert!(
            g.realized_final.unwrap() > delta,
            "delta={delta} should be a profitable triangle"
        );
    }
}

#[test]
fn triangle_gate_reverts_when_unprofitable() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP triangle_gate_reverts: set env");
        return;
    };
    // min_profit far above any achievable net -> the N-leg terminal assert reverts ALL state.
    let g = run_triangle(&arb, &harness, TRIANGLE, 1_000_000, 5_000, 100_000_000);
    assert!(!g.send_ok, "huge min_profit must revert the N-leg cycle");
}
