//! observ-11 — golden-replay regression: predicted (via the LIVE `arb-math` mirror + `CostModel`)
//! vs recorded realized, gated by `tolerance_bps`. The `analytics gate` subcommand exits nonzero on
//! any out-of-tolerance sample so a capital-committing deploy is blocked. No math is forked here:
//! the round-trip is re-priced with `arb_math::RoundTrip` (the same code the bot signs against) and
//! the slippage bps uses `arb_bot::metrics::slippage::slippage_bps`.

use arb_bot::metrics::econ::{CostInputs, CostModel};
use arb_bot::metrics::slippage::slippage_bps;
use arb_math::{CpmmReserves, RoundTrip};
use arb_types::SwapDir;

use crate::corpus::GoldenSample;

/// Per-sample replay verdict.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayResult {
    pub id: String,
    pub predicted_out: u64,
    pub realized_out: u64,
    pub abs_bps_dev: i64,
    pub predicted_e_net: i128,
    pub recorded_net: i64,
    pub within_tolerance: bool,
}

/// Reconstruct the round-trip for a sample (the same orientation the bot would size).
fn round_trip(s: &GoldenSample) -> Option<RoundTrip> {
    let a = CpmmReserves::new(
        s.pool_a.reserve_a,
        s.pool_a.reserve_b,
        s.pool_a.fee_num,
        s.pool_a.fee_den,
    );
    let b = CpmmReserves::new(
        s.pool_b.reserve_a,
        s.pool_b.reserve_b,
        s.pool_b.fee_num,
        s.pool_b.fee_den,
    );
    Some(RoundTrip::new(
        a,
        SwapDir::from_tag(s.dir_a)?,
        b,
        SwapDir::from_tag(s.dir_b)?,
    ))
}

/// Re-price a sample through the live `arb-math` mirror (used to build clean fixtures in tests).
/// `0` if the sample's pools/directions are inconsistent.
#[cfg(test)]
pub fn round_trip_realized_out(s: &GoldenSample) -> u64 {
    round_trip(s)
        .and_then(|rt| rt.realized_out(s.amount_in))
        .unwrap_or(0)
}

/// The economic inputs the CostModel scores for a sample.
pub fn cost_inputs(s: &GoldenSample) -> CostInputs {
    CostInputs {
        spread_lamports: s.spread_lamports,
        swap_fees_lamports: s.swap_fees_lamports,
        flash_fee_lamports: 0,
        tip_lamports: s.tip_lamports,
        prio_lamports: s.prio_lamports,
        base_lamports: s.base_lamports,
        p_land: s.p_land,
    }
}

/// The realized economic net for a sample (base-asset lamports): a landed sample nets
/// `realized_out − amount_in − fees − tip − prio − base`; a loser burns `prio + base`.
pub fn recorded_net(s: &GoldenSample) -> i64 {
    if s.recorded_landed {
        s.recorded_realized_out as i64
            - s.amount_in as i64
            - s.swap_fees_lamports as i64
            - s.tip_lamports as i64
            - s.prio_lamports as i64
            - s.base_lamports as i64
    } else {
        -((s.prio_lamports + s.base_lamports) as i64)
    }
}

/// Replay each sample, comparing predicted_out to recorded realized within `tolerance_bps`.
pub fn replay(
    samples: &[GoldenSample],
    tolerance_bps: i64,
    model: &CostModel,
) -> Vec<ReplayResult> {
    samples
        .iter()
        .map(|s| {
            // A sample the live mirror CANNOT reprice (bad direction tag, inconsistent reserves) must
            // FAIL CLOSED — not collapse to predicted_out=0 / 0 bps and silently pass the deploy gate.
            let priced = round_trip(s).and_then(|rt| rt.realized_out(s.amount_in));
            let predicted_out = priced.unwrap_or(0);
            let abs_bps_dev = slippage_bps(predicted_out, s.recorded_realized_out).abs();
            let within_tolerance = match priced {
                Some(_) => abs_bps_dev <= tolerance_bps,
                None => false, // un-repriceable => block the gate
            };
            ReplayResult {
                id: s.id.clone(),
                predicted_out,
                realized_out: s.recorded_realized_out,
                abs_bps_dev,
                predicted_e_net: model.e_net(&cost_inputs(s)),
                recorded_net: recorded_net(s),
                within_tolerance,
            }
        })
        .collect()
}

/// Whether every sample passed (the CI gate condition).
pub fn all_within_tolerance(results: &[ReplayResult]) -> bool {
    results.iter().all(|r| r.within_tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::sample_fixture;
    use arb_bot::metrics::econ::EconParams;

    /// Fill the recorded_realized_out of each sample from the live mirror so a clean corpus passes.
    fn mirrored_corpus() -> Vec<GoldenSample> {
        let mut c = sample_fixture();
        for s in &mut c {
            let out = round_trip(s)
                .and_then(|rt| rt.realized_out(s.amount_in))
                .unwrap();
            s.recorded_realized_out = out;
        }
        c
    }

    #[test]
    fn clean_corpus_replays_within_zero_tolerance() {
        let model = CostModel::new(EconParams::default());
        let results = replay(&mirrored_corpus(), 0, &model);
        assert!(all_within_tolerance(&results));
        // Bit-exact mirror => 0 bps deviation on every sample.
        assert!(results.iter().all(|r| r.abs_bps_dev == 0));
    }

    #[test]
    fn drift_beyond_tolerance_fails_the_gate() {
        let model = CostModel::new(EconParams::default());
        let mut c = mirrored_corpus();
        // Inject a decode-drift: recorded realized 5% below predicted on the winner.
        c[0].recorded_realized_out = (c[0].recorded_realized_out as f64 * 0.95) as u64;
        let results = replay(&c, 50, &model); // 50 bps tolerance
        assert!(!all_within_tolerance(&results)); // ~500 bps drift trips the gate
    }

    #[test]
    fn loser_recorded_net_is_negative_burn() {
        let c = mirrored_corpus();
        let loser = c.iter().find(|s| !s.recorded_landed).unwrap();
        assert_eq!(
            recorded_net(loser),
            -((loser.prio_lamports + loser.base_lamports) as i64)
        );
    }

    #[test]
    fn un_repriceable_sample_fails_closed_even_at_huge_tolerance() {
        let model = CostModel::new(EconParams::default());
        let mut c = mirrored_corpus();
        // Corrupt the direction tag so the live mirror cannot reprice it.
        c[0].dir_a = 2; // invalid SwapDir tag => round_trip None
        let results = replay(&c, 1_000_000, &model); // absurdly generous tolerance
        let bad = results.iter().find(|r| r.id == c[0].id).unwrap();
        assert_eq!(bad.predicted_out, 0);
        assert!(
            !bad.within_tolerance,
            "un-repriceable sample must FAIL the gate"
        );
        assert!(!all_within_tolerance(&results)); // whole gate blocks
    }
}
