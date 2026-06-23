//! M1-GATE Token-2022 fee-only path (onchain-10 / sizing-9 / testing-5 "Done when"). The
//! intermediate token carries a Token-2022 receipt transfer fee, so leg A's output is skimmed.
//! The processor must feed the MEASURED (post-skim) intermediate delta into leg B, and the
//! off-chain prediction — `arb_math::fees::amount_after_fee` applied between the legs — must
//! equal the on-chain realized base out, bit-for-bit, across fee tiers and sizes.
//!
//! NOTE: this models the fee on the intermediate RECEIPT (the dominant, clearly-measured skim),
//! proving the processor's measure-actual-delta invariant. The symmetric send-side fee and
//! bit-exactness vs the real spl-token-2022 program are completed on Surfpool (Class B).

mod common;
use arb_math::fees::TransferFeeConfig;
use common::*;

/// Off-chain predicted final base out when the intermediate carries a receipt fee:
/// leg A gross out → skim → net intermediate → leg B → base.
fn predicted_final_t22(cfg: &GateCfg, bps: u16, max: u64) -> Option<u64> {
    let mid_gross = cfg
        .pool_a
        .reserves()
        .quote_out(cfg.pool_a.dir, cfg.delta_in)?;
    let fee = TransferFeeConfig {
        transfer_fee_basis_points: bps,
        maximum_fee: max,
    };
    let net_mid = fee.amount_after_fee(mid_gross)?;
    cfg.pool_b.reserves().quote_out(cfg.pool_b.dir, net_mid)
}

#[test]
fn token2022_receipt_fee_path_differential() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP token2022_receipt_fee_path_differential: set env");
        return;
    };
    let dex = allowlisted_dex();
    let mut checked = 0u32;
    for (bps, max) in [(30u16, u64::MAX), (100, u64::MAX), (250, 5_000u64)] {
        for delta in [2_000u64, 10_000, 40_000] {
            let cfg = GateCfg {
                inter_recv_fee: Some((bps, max)),
                ..GateCfg::profitable().with_delta(delta)
            };
            let g = run_roundtrip(&arb, &harness, dex, &cfg);
            let predicted = predicted_final_t22(&cfg, bps, max);

            if g.send_ok {
                assert_eq!(
                    g.realized_final, predicted,
                    "T22 DRIFT bps={bps} max={max} delta={delta}: realized {:?} != predicted {:?}",
                    g.realized_final, predicted
                );
                assert!(
                    matches!(predicted, Some(f) if f >= cfg.delta_in),
                    "T22 bps={bps} delta={delta}: succeeded but off-chain predicted unprofitable"
                );
            } else {
                let unprofitable =
                    matches!(predicted, Some(f) if f < cfg.delta_in) || predicted.is_none();
                assert!(
                    unprofitable,
                    "T22 bps={bps} delta={delta}: reverted ({:?}) but predicted profitable {:?}",
                    g.err_code, predicted
                );
                if let Some(code) = g.err_code {
                    assert_eq!(
                        code, 6000,
                        "T22 bps={bps} delta={delta}: expected Unprofitable(6000), got {code}"
                    );
                }
            }
            checked += 1;
        }
    }
    eprintln!(
        "Token-2022 receipt-fee path GREEN: {checked} cases, realized net == predicted \
         (amount_after_fee mirror), bit-exact"
    );
}
