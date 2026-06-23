//! M1-GATE rounding-mirror fuzz/property gate (onchain-10 + sizing-9 + testing-5, Raydium-CPMM
//! venue, BOTH directions, swept over reserves + fee + amount_in). For each random case the
//! off-chain `arb_math` prediction MUST equal the on-chain CPI's realized balance delta
//! bit-for-bit on success, and the program MUST revert (Unprofitable) exactly when the
//! prediction is non-profitable. A divergence is shrunk to a minimal counterexample and fails
//! the build. (Orca sqrt-price + real-venue rounding are proven on Surfpool — Class B.)

mod common;
use arb_types::SwapDir;
use common::*;

/// Number of fuzz cases. Each is a fresh SVM + program load + tx (~80ms), so this is bounded;
/// the off-chain-only property sweep below covers a far wider range cheaply.
const CASES: u64 = 256;

fn orient(dir: SwapDir, r_in: u64, r_out: u64) -> (u64, u64) {
    match dir {
        SwapDir::AtoB => (r_in, r_out),
        SwapDir::BtoA => (r_out, r_in),
    }
}

fn random_fee(rng: &mut Lcg) -> (u64, u64) {
    let fee_den = match rng.next_u64() % 3 {
        0 => 10_000u64,
        1 => 100_000,
        _ => 1_000_000,
    };
    // 0 .. 5% fee.
    let fee_num = rng.range(0, fee_den / 20 + 1);
    (fee_num, fee_den)
}

/// A random round-trip: 70% "constructed edge" family (a price dislocation, so a mix of
/// profitable + oversized-unprofitable trades), 30% fully-random reserves. Direction of each
/// leg is chosen independently and the reserves oriented accordingly.
fn random_cfg(rng: &mut Lcg, dex: solana_sdk::pubkey::Pubkey) -> GateCfg {
    let scale = rng.range(1, 2000);
    let (ra_in, ra_out, rb_in, rb_out) = if rng.next_u64() % 10 < 7 {
        let ra_in = 1_000_000u64.saturating_mul(scale);
        let ra_out = 2_000_000u64.saturating_mul(scale);
        let rb_in = 2_000_000u64.saturating_mul(scale);
        let rb_out = rng.range(800_000, 1_500_000).saturating_mul(scale);
        (ra_in, ra_out, rb_in, rb_out)
    } else {
        let a = rng.range(50_000, 5_000_000).saturating_mul(scale.max(1));
        let b = rng.range(50_000, 5_000_000).saturating_mul(scale.max(1));
        let c = rng.range(50_000, 5_000_000).saturating_mul(scale.max(1));
        let d = rng.range(50_000, 5_000_000).saturating_mul(scale.max(1));
        (a, b, c, d)
    };
    let dir_a = rng.dir();
    let dir_b = rng.dir();
    let (fn_a, fd_a) = random_fee(rng);
    let (fn_b, fd_b) = random_fee(rng);
    let (a_a, a_b) = orient(dir_a, ra_in, ra_out);
    let (b_a, b_b) = orient(dir_b, rb_in, rb_out);
    let delta_in = rng.range(100, (ra_in / 8).max(200));

    GateCfg {
        pool_a: PoolCfg::new(a_a, a_b, dir_a).with_fee(fn_a, fd_a),
        pool_b: PoolCfg::new(b_a, b_b, dir_b).with_fee(fn_b, fd_b),
        base_funding: 2_000_000_000_000_000, // >> any delta; leg-A debit never underflows
        delta_in,
        min_profit: 0,
        leg_dex: dex,
        balance_owner_override: None,
        strand_intermediate: false,
        inter_recv_fee: None,
    }
}

fn describe(cfg: &GateCfg) -> String {
    format!(
        "pool_a=({},{},fee {}/{},{:?}) pool_b=({},{},fee {}/{},{:?}) delta={}",
        cfg.pool_a.reserve_a,
        cfg.pool_a.reserve_b,
        cfg.pool_a.fee_num,
        cfg.pool_a.fee_den,
        cfg.pool_a.dir,
        cfg.pool_b.reserve_a,
        cfg.pool_b.reserve_b,
        cfg.pool_b.fee_num,
        cfg.pool_b.fee_den,
        cfg.pool_b.dir,
        cfg.delta_in,
    )
}

/// On a detected drift, shrink `delta_in` (halving) to the smallest value that still diverges
/// and panic with that minimal counterexample (testing-5: minimal-counterexample requirement).
fn shrink_and_panic(
    arb: &[u8],
    harness: &[u8],
    dex: solana_sdk::pubkey::Pubkey,
    cfg: &GateCfg,
) -> ! {
    let mut minimal = *cfg;
    let mut d = cfg.delta_in;
    while d > 1 {
        let cand = GateCfg {
            delta_in: d / 2,
            ..*cfg
        };
        let g = run_roundtrip(arb, harness, dex, &cand);
        let diverges = g.send_ok && g.realized_final != g.predicted_final;
        if diverges {
            minimal = cand;
            d /= 2;
        } else {
            break;
        }
    }
    let g = run_roundtrip(arb, harness, dex, &minimal);
    panic!(
        "M1-GATE DRIFT (shrunk): realized {:?} != predicted {:?} | {}",
        g.realized_final,
        g.predicted_final,
        describe(&minimal)
    );
}

#[test]
fn rounding_mirror_fuzz_both_directions_and_fees() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP rounding_mirror_fuzz: set ARB_PROGRAM_SO + SWAP_HARNESS_SO");
        return;
    };
    let dex = allowlisted_dex();
    let mut rng = Lcg::new(0xA1B2_C3D4_E5F6_0917);
    let mut n_success = 0u64;
    let mut n_revert = 0u64;

    for i in 0..CASES {
        let cfg = random_cfg(&mut rng, dex);
        let g = run_roundtrip(&arb, &harness, dex, &cfg);
        let predicted = g
            .predicted_final
            .expect("bounded inputs never overflow off-chain");
        let should_succeed = predicted >= cfg.delta_in; // min_profit = 0

        if g.send_ok {
            if g.realized_final != Some(predicted) {
                shrink_and_panic(&arb, &harness, dex, &cfg);
            }
            assert!(
                should_succeed,
                "case {i}: on-chain succeeded but off-chain predicted unprofitable | {}",
                describe(&cfg)
            );
            n_success += 1;
        } else {
            assert!(
                !should_succeed,
                "case {i}: reverted (code {:?}) but off-chain predicted profitable ({}) | {}",
                g.err_code,
                predicted,
                describe(&cfg)
            );
            if let Some(code) = g.err_code {
                assert_eq!(
                    code,
                    6000,
                    "case {i}: expected Unprofitable(6000) revert, got {code} | {}",
                    describe(&cfg)
                );
            }
            n_revert += 1;
        }
    }

    assert!(
        n_success > 20,
        "fuzz degenerate: only {n_success} profitable cases"
    );
    assert!(
        n_revert > 20,
        "fuzz degenerate: only {n_revert} revert cases"
    );
    eprintln!(
        "rounding-mirror fuzz GREEN: {CASES} cases ({n_success} profit / {n_revert} revert), \
         0 drift, both directions + fee sweep (Raydium CPMM venue)"
    );
}

/// Explicit coverage of all four (dir_a, dir_b) orientations over the canonical profitable edge,
/// so BtoA is exercised deterministically regardless of the fuzz RNG path.
#[test]
fn differential_all_four_direction_orientations() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP differential_all_four_direction_orientations: set env");
        return;
    };
    let dex = allowlisted_dex();
    for dir_a in [SwapDir::AtoB, SwapDir::BtoA] {
        for dir_b in [SwapDir::AtoB, SwapDir::BtoA] {
            for delta in [1_000u64, 5_000, 25_000] {
                let (a_a, a_b) = orient(dir_a, 1_000_000, 2_000_000);
                let (b_a, b_b) = orient(dir_b, 2_000_000, 1_100_000);
                let cfg = GateCfg {
                    pool_a: PoolCfg::new(a_a, a_b, dir_a),
                    pool_b: PoolCfg::new(b_a, b_b, dir_b),
                    base_funding: 1_000_000,
                    delta_in: delta,
                    min_profit: 0,
                    leg_dex: dex,
                    balance_owner_override: None,
                    strand_intermediate: false,
                    inter_recv_fee: None,
                };
                let g = run_roundtrip(&arb, &harness, dex, &cfg);
                assert!(
                    g.send_ok,
                    "{:?}/{:?} delta={delta}: expected profitable success, got revert {:?}",
                    dir_a, dir_b, g.err_code
                );
                assert_eq!(
                    g.realized_final, g.predicted_final,
                    "{:?}/{:?} delta={delta}: DRIFT realized {:?} != predicted {:?}",
                    dir_a, dir_b, g.realized_final, g.predicted_final
                );
            }
        }
    }
    eprintln!("all four direction orientations: realized == predicted, bit-exact");
}
