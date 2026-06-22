//! observ-6 — health evaluator + numeric kill-switch thresholds.
//!
//! Computes a [`HealthSnapshot`] from the timestamped [`PnlLedger`] windows + the cumulative
//! [`MetricsRegistry`], and emits [`KillSwitchSignal::Trip`] when any numeric threshold breaches.
//! This is the metric SIDE of the kill-switch: it produces the signal; the signer module owns the
//! `signing-enabled` flag it flips (plan §9, observ-14 seam). `evaluate` is read-only over the
//! ledger/registry (it never mutates) and takes an explicit `now_millis` so trips are deterministic.

use super::latency::{ConfirmationRank, LatencyStage};
use super::pnl::PnlLedger;
use super::registry::MetricsRegistry;

/// Numeric kill-switch thresholds (config-overridable). Defaults match plan §9.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// Revert-rate trip threshold (percent). §9: >30% ⇒ infra bug.
    pub revert_rate_pct: f64,
    /// Window over which revert-rate is computed.
    pub revert_window_millis: u64,
    /// Minimum resolved attempts in the window before revert-rate can trip (no low-sample trip).
    pub revert_min_attempts: u64,
    /// Burn-rate trip threshold (lamports/min on reverted losers).
    pub burn_rate_lamports_per_min_max: u64,
    /// Window over which burn-rate is computed.
    pub burn_window_millis: u64,
    /// Realized-loss trip threshold (SOL/hour).
    pub realized_loss_sol_per_hour_max: f64,
    /// Window over which realized loss is computed.
    pub realized_loss_window_millis: u64,
    /// Hot-key balance deviation trip threshold (percent of expected).
    pub balance_dev_pct: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            revert_rate_pct: 30.0,
            revert_window_millis: 300_000, // 5 min
            revert_min_attempts: 20,
            burn_rate_lamports_per_min_max: 50_000_000, // 0.05 SOL/min
            burn_window_millis: 60_000,
            realized_loss_sol_per_hour_max: 0.5,
            realized_loss_window_millis: 3_600_000, // 1 hr
            balance_dev_pct: 15.0,
        }
    }
}

/// Why the health evaluator wants to halt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TripReason {
    RevertRateExceeded { pct: f64 },
    BurnRateExceeded { lamports_per_min: u64 },
    RealizedLossExceeded { sol_per_hour: f64 },
    BalanceDeviationExceeded { pct: f64 },
}

/// Health → kill-switch signal consumed by the signer supervisor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KillSwitchSignal {
    Healthy,
    Trip { reason: TripReason },
}

impl KillSwitchSignal {
    pub fn is_trip(&self) -> bool {
        matches!(self, KillSwitchSignal::Trip { .. })
    }
}

/// Canonical dashboard / `/healthz` payload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HealthSnapshot {
    pub revert_rate_pct: f64,
    pub burn_rate_lpm: u64,
    pub realized_loss_sol_per_hour: f64,
    pub submit_p50_ms: Option<f64>,
    pub submit_p95_ms: Option<f64>,
    pub realized_pnl_lamports: i64,
    pub attempts: u64,
    pub lands: u64,
    pub reverts: u64,
    pub last_confirmation: Option<ConfirmationRank>,
}

/// Stateless evaluator over the ledger + registry.
#[derive(Clone, Copy, Debug, Default)]
pub struct HealthEvaluator {
    pub thresholds: Thresholds,
}

impl HealthEvaluator {
    pub fn new(thresholds: Thresholds) -> Self {
        Self { thresholds }
    }

    /// Read-only health snapshot for `/healthz` and dashboards.
    pub fn snapshot(
        &self,
        pnl: &PnlLedger,
        reg: &MetricsRegistry,
        now_millis: u64,
    ) -> HealthSnapshot {
        let t = &self.thresholds;
        let counters = reg.snapshot();
        HealthSnapshot {
            revert_rate_pct: pnl.revert_rate_pct_window(t.revert_window_millis, now_millis),
            burn_rate_lpm: pnl.burn_rate_lamports_per_min(t.burn_window_millis, now_millis),
            realized_loss_sol_per_hour: pnl
                .realized_loss_sol_per_hour(t.realized_loss_window_millis, now_millis),
            submit_p50_ms: reg.latency.p50_ms(LatencyStage::SubmitToLand),
            submit_p95_ms: reg.latency.p95_ms(LatencyStage::SubmitToLand),
            realized_pnl_lamports: pnl.realized_pnl_lifetime(),
            attempts: counters.attempts,
            lands: counters.lands,
            reverts: counters.reverts,
            last_confirmation: reg.latency.last_confirmation(),
        }
    }

    /// Evaluate thresholds and return the first trip (or `Healthy`). Read-only.
    pub fn evaluate(
        &self,
        pnl: &PnlLedger,
        _reg: &MetricsRegistry,
        hot_key_balance: u64,
        expected_balance: u64,
        now_millis: u64,
    ) -> KillSwitchSignal {
        let t = &self.thresholds;

        // 1. Revert-rate — gated by a minimum sample count so a single early revert never trips.
        let (lands, reverts) = pnl.counts_window(t.revert_window_millis, now_millis);
        let resolved = lands + reverts;
        if resolved >= t.revert_min_attempts {
            let pct = reverts as f64 * 100.0 / resolved as f64;
            if pct > t.revert_rate_pct {
                return KillSwitchSignal::Trip {
                    reason: TripReason::RevertRateExceeded { pct },
                };
            }
        }

        // 2. Realized loss / hour.
        let loss = pnl.realized_loss_sol_per_hour(t.realized_loss_window_millis, now_millis);
        if loss > t.realized_loss_sol_per_hour_max {
            return KillSwitchSignal::Trip {
                reason: TripReason::RealizedLossExceeded { sol_per_hour: loss },
            };
        }

        // 3. Hot-key balance deviation (suspected compromise / leak).
        if expected_balance > 0 {
            let dev = (hot_key_balance as f64 - expected_balance as f64).abs() * 100.0
                / expected_balance as f64;
            if dev > t.balance_dev_pct {
                return KillSwitchSignal::Trip {
                    reason: TripReason::BalanceDeviationExceeded { pct: dev },
                };
            }
        }

        // 4. Burn-rate.
        let burn = pnl.burn_rate_lamports_per_min(t.burn_window_millis, now_millis);
        if burn > t.burn_rate_lamports_per_min_max {
            return KillSwitchSignal::Trip {
                reason: TripReason::BurnRateExceeded {
                    lamports_per_min: burn,
                },
            };
        }

        KillSwitchSignal::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::pnl::TxOutcome;
    use solana_pubkey::Pubkey;

    fn tok() -> Pubkey {
        Pubkey::new_from_array([7; 32])
    }

    fn healthy_thresholds() -> Thresholds {
        Thresholds::default()
    }

    #[test]
    fn revert_rate_trips_above_30pct_with_enough_samples() {
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let ev = HealthEvaluator::new(healthy_thresholds());
        // 25 attempts, 10 lands + 15 reverts => 60% revert-rate (> 30%), >= 20 attempts.
        for _ in 0..10 {
            pnl.record_outcome(TxOutcome::landed(tok(), 10, 0, 0, 0, 1_000))
                .unwrap();
        }
        for _ in 0..15 {
            pnl.record_outcome(TxOutcome::reverted_onchain(tok(), 1, 1, 1_000))
                .unwrap();
        }
        let sig = ev.evaluate(&pnl, &reg, 1_000, 1_000, 1_000);
        assert!(matches!(
            sig,
            KillSwitchSignal::Trip {
                reason: TripReason::RevertRateExceeded { .. }
            }
        ));
    }

    #[test]
    fn revert_rate_does_not_trip_below_min_attempts() {
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let ev = HealthEvaluator::new(healthy_thresholds());
        // 3 reverts only (100% rate but < revert_min_attempts=20) => no trip.
        for _ in 0..3 {
            pnl.record_outcome(TxOutcome::reverted_onchain(tok(), 1, 1, 1_000))
                .unwrap();
        }
        assert_eq!(
            ev.evaluate(&pnl, &reg, 1_000, 1_000, 1_000),
            KillSwitchSignal::Healthy
        );
    }

    #[test]
    fn burn_rate_trips_at_boundary() {
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let mut th = healthy_thresholds();
        th.burn_rate_lamports_per_min_max = 10_000;
        th.burn_window_millis = 60_000;
        let ev = HealthEvaluator::new(th);
        // 20_000 lamports burned over 60s => 20_000 lpm > 10_000 max.
        pnl.record_outcome(TxOutcome::reverted_onchain(tok(), 10_000, 10_000, 50_000))
            .unwrap();
        let sig = ev.evaluate(&pnl, &reg, 1_000, 1_000, 100_000);
        assert!(matches!(
            sig,
            KillSwitchSignal::Trip {
                reason: TripReason::BurnRateExceeded { .. }
            }
        ));
    }

    #[test]
    fn balance_deviation_trips() {
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let ev = HealthEvaluator::new(healthy_thresholds());
        // expected 1_000_000, actual 800_000 => 20% deviation > 15%.
        let sig = ev.evaluate(&pnl, &reg, 800_000, 1_000_000, 1);
        assert!(matches!(
            sig,
            KillSwitchSignal::Trip {
                reason: TripReason::BalanceDeviationExceeded { .. }
            }
        ));
    }

    #[test]
    fn healthy_when_all_within_bounds() {
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let ev = HealthEvaluator::new(healthy_thresholds());
        // The executor feeds both books: the registry (cumulative counters) and the PnL ledger
        // (windowed economics). The snapshot's lands/attempts come from the registry.
        reg.record_attempt();
        reg.record_land(100_000, 10, 0);
        pnl.record_outcome(TxOutcome::landed(tok(), 100_000, 1_000, 5_000, 2_000, 1))
            .unwrap();
        let sig = ev.evaluate(&pnl, &reg, 1_000_000, 1_000_000, 2);
        assert_eq!(sig, KillSwitchSignal::Healthy);
        let snap = ev.snapshot(&pnl, &reg, 2);
        assert_eq!(snap.lands, 1);
        // Net of the land's own prio(1_000)+base(5_000)+tip(2_000): 100_000 − 8_000 = 92_000.
        assert_eq!(snap.realized_pnl_lamports, 92_000);
    }
}
