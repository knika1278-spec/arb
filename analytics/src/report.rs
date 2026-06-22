//! Render replay/backtest results to a human-readable report + a CI-friendly summary line.

use crate::backtest::BacktestReport;
use crate::replay::ReplayResult;

/// Markdown report of a replay run.
pub fn render_replay(results: &[ReplayResult]) -> String {
    let passed = results.iter().filter(|r| r.within_tolerance).count();
    let mut out = format!(
        "# Golden-replay gate\n\n{passed}/{} samples within tolerance\n\n",
        results.len()
    );
    for r in results {
        out.push_str(&format!(
            "- {:<14} predicted_out={:<12} realized_out={:<12} dev={:>5}bps e_net={:<10} {}\n",
            r.id,
            r.predicted_out,
            r.realized_out,
            r.abs_bps_dev,
            r.predicted_e_net,
            if r.within_tolerance { "PASS" } else { "FAIL" }
        ));
    }
    out.push_str(&format!(
        "\n{}\n",
        if passed == results.len() {
            "GATE PASS"
        } else {
            "GATE FAIL"
        }
    ));
    out
}

/// A single CI-parseable summary line for a replay run.
pub fn replay_summary_line(results: &[ReplayResult]) -> String {
    let passed = results.iter().filter(|r| r.within_tolerance).count();
    let worst = results.iter().map(|r| r.abs_bps_dev).max().unwrap_or(0);
    format!(
        "replay: {}/{} pass, worst_dev={}bps, gate={}",
        passed,
        results.len(),
        worst,
        if passed == results.len() {
            "PASS"
        } else {
            "FAIL"
        }
    )
}

/// Markdown + JSON-ish report of a backtest run.
pub fn render_backtest(r: &BacktestReport) -> String {
    format!(
        "# Backtest (unit-economics confirmation)\n\n\
         samples              = {}\n\
         predicted_e_net_total= {}\n\
         realized_net_total   = {}\n\
         realized_revert_rate = {:.1}%\n\
         realized_burn        = {} lamports\n\
         model_bias           = {}  (predicted - realized)\n",
        r.samples,
        r.predicted_e_net_total,
        r.realized_net_total,
        r.realized_revert_rate_pct,
        r.realized_burn_lamports,
        r.model_bias,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::ReplayResult;

    fn result(id: &str, within: bool, dev: i64) -> ReplayResult {
        ReplayResult {
            id: id.into(),
            predicted_out: 100,
            realized_out: 100,
            abs_bps_dev: dev,
            predicted_e_net: 10,
            recorded_net: 5,
            within_tolerance: within,
        }
    }

    #[test]
    fn report_banner_reflects_pass_fail() {
        let pass = vec![result("a", true, 0), result("b", true, 3)];
        assert!(render_replay(&pass).contains("GATE PASS"));
        let fail = vec![result("a", true, 0), result("b", false, 700)];
        let r = render_replay(&fail);
        assert!(r.contains("GATE FAIL"));
        assert!(r.contains("FAIL"));
    }

    #[test]
    fn summary_line_reports_worst_dev() {
        let res = vec![result("a", true, 12), result("b", false, 800)];
        let line = replay_summary_line(&res);
        assert!(line.contains("worst_dev=800bps"));
        assert!(line.contains("gate=FAIL"));
    }
}
