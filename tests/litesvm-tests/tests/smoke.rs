//! Fase-0 smoke: the workspace + shared crates wire together and resolve from `arb-config`
//! (no hardcoded ids in tests). The real revert proof (`FailedTransactionMetadata` on a
//! deliberately-unprofitable input) is a Fase-1 task that loads the build-sbf `.so`.

#[test]
fn allowlist_and_limits_resolve_from_config() {
    assert_eq!(arb_config::WAVE1_DEX_ALLOWLIST.len(), 3);
    assert_eq!(arb_config::MAX_TX_ACCOUNT_LOCKS, 128);
    assert_eq!(arb_config::TX_SIZE_LIMIT_BYTES, 1232);
    assert!(litesvm_tests::substrate_ready());
}

#[test]
fn math_engine_links_and_prices_a_known_swap() {
    // 1_000_000/1_000_000 pool, 25bps, in=10_000 -> 9876 (hand-verified in arb-math).
    let p = arb_math::CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
    assert_eq!(p.quote_out(arb_types::SwapDir::AtoB, 10_000), Some(9876));
}
