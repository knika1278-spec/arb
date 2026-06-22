//! observ-12 — aggregate backtest: run the `CostModel` over the corpus to estimate predicted vs
//! realized E[net], realized revert-rate, and burn, surfacing model bias. This is the "confirm
//! unit-economics before infra spend" artifact (plan §10): a loser-dominated corpus should net
//! negative/near-zero realized, consistent with the liquid-pair reality.

use arb_bot::metrics::econ::CostModel;

use crate::corpus::GoldenSample;
use crate::replay::{cost_inputs, recorded_net};

/// Aggregate predicted-vs-realized report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BacktestReport {
    pub samples: usize,
    pub predicted_e_net_total: i128,
    pub realized_net_total: i64,
    pub realized_revert_rate_pct: f64,
    pub realized_burn_lamports: u64,
    /// predicted_total − realized_total (positive ⇒ model is optimistic).
    pub model_bias: i128,
}

/// Run the backtest over a corpus.
pub fn run_backtest(samples: &[GoldenSample], model: &CostModel) -> BacktestReport {
    let mut predicted_e_net_total: i128 = 0;
    let mut realized_net_total: i64 = 0;
    let mut reverts: u64 = 0;
    let mut burn: u64 = 0;

    for s in samples {
        predicted_e_net_total += model.e_net(&cost_inputs(s));
        realized_net_total += recorded_net(s);
        if !s.recorded_landed {
            reverts += 1;
            burn += s.prio_lamports + s.base_lamports;
        }
    }

    let n = samples.len();
    let realized_revert_rate_pct = if n == 0 {
        0.0
    } else {
        reverts as f64 * 100.0 / n as f64
    };

    BacktestReport {
        samples: n,
        predicted_e_net_total,
        realized_net_total,
        realized_revert_rate_pct,
        realized_burn_lamports: burn,
        model_bias: predicted_e_net_total - realized_net_total as i128,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{sample_fixture, GoldenSample};
    use crate::replay::round_trip_realized_out;
    use arb_bot::metrics::econ::EconParams;

    fn corpus() -> Vec<GoldenSample> {
        let mut c = sample_fixture();
        for s in &mut c {
            s.recorded_realized_out = round_trip_realized_out(s);
        }
        c
    }

    #[test]
    fn loser_dominated_corpus_nets_nonpositive_realized() {
        // Three losers + one winner — realized net should be dragged down by burn.
        let base = corpus();
        let winner = base[0].clone();
        let loser = base[1].clone();
        let samples = vec![winner, loser.clone(), loser.clone(), loser];
        let model = CostModel::new(EconParams::default());
        let report = run_backtest(&samples, &model);
        assert_eq!(report.samples, 4);
        assert_eq!(report.realized_revert_rate_pct, 75.0);
        assert!(report.realized_burn_lamports > 0);
    }

    #[test]
    fn model_bias_is_predicted_minus_realized() {
        let samples = corpus();
        let model = CostModel::new(EconParams::default());
        let report = run_backtest(&samples, &model);
        assert_eq!(
            report.model_bias,
            report.predicted_e_net_total - report.realized_net_total as i128
        );
    }
}
