//! testing-4 — LiteSVM unit tests: revert + exact-delta + boundary.
//! Proves: an unprofitable round-trip reverts (Unprofitable) with zero net token movement; a
//! profitable round-trip succeeds with the realized base delta EXACTLY equal to the off-chain
//! prediction; and the `min_profit` boundary is tight (succeeds at predicted profit, reverts one
//! unit above).

mod common;
use arb_types::SwapDir;
use common::*;

#[test]
fn unprofitable_reverts_with_no_net_movement() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP unprofitable_reverts_with_no_net_movement: set env");
        return;
    };
    let dex = allowlisted_dex();
    // A round-trip through two equal (balanced) pools must lose to fees -> Unprofitable.
    let cfg = GateCfg {
        pool_a: PoolCfg::new(1_000_000, 1_000_000, SwapDir::AtoB),
        pool_b: PoolCfg::new(1_000_000, 1_000_000, SwapDir::AtoB),
        base_funding: 1_000_000,
        delta_in: 10_000,
        min_profit: 0,
        leg_dex: dex,
        balance_owner_override: None,
        strand_intermediate: false,
        inter_recv_fee: None,
    };
    let g = run_roundtrip(&arb, &harness, dex, &cfg);
    assert!(!g.send_ok, "balanced round-trip must revert, got success");
    assert_eq!(
        g.err_code,
        Some(6000),
        "revert must be Unprofitable(6000), got {:?}",
        g.err_code
    );
    // Zero net token movement: the runtime reverted ALL state.
    assert_eq!(
        g.base_balance, cfg.base_funding,
        "base ATA must be untouched after revert"
    );
    assert_eq!(
        g.inter_balance, 0,
        "intermediate ATA must be untouched after revert"
    );
}

#[test]
fn profitable_success_realizes_exact_predicted_delta() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP profitable_success_realizes_exact_predicted_delta: set env");
        return;
    };
    let dex = allowlisted_dex();
    let cfg = GateCfg::profitable().with_delta(5_000);
    let g = run_roundtrip(&arb, &harness, dex, &cfg);
    assert!(
        g.send_ok,
        "profitable round-trip must succeed, got {:?}",
        g.err_code
    );
    assert_eq!(
        g.realized_final, g.predicted_final,
        "realized base out {:?} != off-chain predicted {:?}",
        g.realized_final, g.predicted_final
    );
    let realized_profit = g.realized_final.unwrap() as i128 - cfg.delta_in as i128;
    assert_eq!(
        realized_profit,
        predicted_profit(&cfg).unwrap(),
        "realized profit != predicted profit"
    );
    assert!(realized_profit > 0, "fixture should be profitable");
}

#[test]
fn min_profit_boundary_is_tight() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP min_profit_boundary_is_tight: set env");
        return;
    };
    let dex = allowlisted_dex();
    let base = GateCfg::profitable().with_delta(5_000);
    let p = predicted_profit(&base).expect("finite");
    assert!(p > 0, "fixture must be profitable, got {p}");
    let p = p as u64;

    // Exactly meeting the predicted profit succeeds: post_base == pre + p == required.
    let g_ok = run_roundtrip(&arb, &harness, dex, &base.with_min_profit(p));
    assert!(
        g_ok.send_ok,
        "min_profit == predicted profit ({p}) must succeed, got revert {:?}",
        g_ok.err_code
    );

    // One unit above the predicted profit reverts as Unprofitable.
    let g_rev = run_roundtrip(&arb, &harness, dex, &base.with_min_profit(p + 1));
    assert!(
        !g_rev.send_ok,
        "min_profit == predicted+1 ({}) must revert",
        p + 1
    );
    assert_eq!(
        g_rev.err_code,
        Some(6000),
        "boundary revert must be Unprofitable(6000), got {:?}",
        g_rev.err_code
    );
}
