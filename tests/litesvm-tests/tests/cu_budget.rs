//! onchain-9 / testing-7 (CU half) + add-5 — compute-unit budget is MEASURED from the live
//! LiteSVM execution (never hardcoded) and the 2-leg round-trip stays strictly under the
//! 1.4M MAX_COMPUTE_UNIT_LIMIT. The account-locks(<128) / tx-bytes(<=1232) / ALT-prewarm halves
//! of testing-7 are off-chain budget asserts owned by the tx-builder (txbuilder-6/9, host-green).

mod common;
use common::*;

#[test]
fn round_trip_compute_units_measured_and_under_budget() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP round_trip_compute_units_measured_and_under_budget: set env");
        return;
    };
    let dex = allowlisted_dex();
    let cfg = GateCfg::profitable().with_delta(5_000);
    let g = run_roundtrip(&arb, &harness, dex, &cfg);
    assert!(
        g.send_ok,
        "fixture must succeed to measure CU, got revert {:?}",
        g.err_code
    );
    let cu = g.cu.expect("CU is measured on a successful tx");
    eprintln!("TryArbitrage 2-leg round-trip consumed {cu} CU (cap 1_400_000)");

    // add-5: the budget is a real runtime MEASUREMENT, not a hardcoded/defaulted value.
    assert!(
        cu > 0,
        "CU must be measured (>0), got 0 — would mean a defaulted value"
    );
    // onchain-9 / testing-7: total tx CU strictly under MAX_COMPUTE_UNIT_LIMIT.
    assert!(
        cu < 1_400_000,
        "total CU {cu} exceeds MAX_COMPUTE_UNIT_LIMIT 1_400_000"
    );
    // Regression sanity: a 2-leg constant-product round-trip is cheap vs the cap.
    assert!(
        cu < 200_000,
        "2-leg round-trip CU {cu} unexpectedly high — possible regression"
    );
}
