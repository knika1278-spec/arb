//! observ-14 — the metrics↔signer integration seam: the synchronous pre-sign gate.
//!
//! **Contract (single source of truth, no duplication):**
//! - the *metrics* side OWNS the economics + health LOGIC — [`CostModel::gate`] (the probabilistic
//!   `E[net]` cost-gate, observ-4) and [`crate::metrics::health::HealthEvaluator::evaluate`] →
//!   [`crate::metrics::health::KillSwitchSignal`] (observ-6);
//! - the *signer* side OWNS the STATE — the `signing-enabled` flag ([`KillSwitchHandle`], signer-6)
//!   and the synchronous lamport-out cap ([`super::caps::PreSignCaps`], signer-4).
//!
//! This module is the thin seam that wires the two: a single synchronous, allocation-free decision
//! ([`evaluate_pre_sign`]) the caller runs immediately before the sidecar's atomic sign path, and a
//! health-signal route ([`super::killswitch::apply_health_signal`], signer-7) that flips the flag on
//! a `Trip`. Neither the EV math nor the flag is re-implemented here — they are only composed.
//!
//! Order is deliberate: the signer-owned flag is checked FIRST (a halted bot short-circuits before
//! any economic evaluation), then the metrics cost-gate. The lamport-out CAP stays inside the
//! sidecar's mutex-held sequence (`flag → shape → caps → sign`); this gate is the economic
//! last-look that precedes it.

use super::killswitch::KillSwitchHandle;
use crate::metrics::econ::{CostGateDecision, CostInputs, CostModel, RejectReason};

/// Outcome of the synchronous pre-sign gate, evaluated before the hot key is ever touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreSignDecision {
    /// Both gates pass — the caller may proceed to the sidecar sign path.
    Proceed { e_net_lamports: i128 },
    /// The kill-switch flag is off — do not sign (no economic evaluation performed).
    Halted,
    /// The metrics cost-gate vetoed the opportunity.
    CostRejected {
        reason: RejectReason,
        e_net_lamports: i128,
    },
}

impl PreSignDecision {
    /// Whether the caller may proceed to sign.
    pub fn may_sign(&self) -> bool {
        matches!(self, PreSignDecision::Proceed { .. })
    }
}

/// Run the synchronous pre-sign gate: signer-owned flag FIRST, then the metrics cost-gate. Both
/// inputs are borrowed from their owning modules; nothing is recomputed here.
pub fn evaluate_pre_sign(
    flag: &KillSwitchHandle,
    cost_model: &CostModel,
    inputs: &CostInputs,
) -> PreSignDecision {
    if !flag.signing_enabled() {
        return PreSignDecision::Halted;
    }
    match cost_model.gate(inputs) {
        CostGateDecision::Proceed { e_net_lamports } => PreSignDecision::Proceed { e_net_lamports },
        CostGateDecision::Reject {
            reason,
            e_net_lamports,
        } => PreSignDecision::CostRejected {
            reason,
            e_net_lamports,
        },
    }
}

/// Ergonomic handle bundling the two borrowed seam endpoints so a hot loop can re-evaluate many
/// opportunities without re-wiring them each call.
#[derive(Clone, Copy)]
pub struct PreSignGate<'a> {
    pub flag: &'a KillSwitchHandle,
    pub cost_model: &'a CostModel,
}

impl<'a> PreSignGate<'a> {
    pub fn new(flag: &'a KillSwitchHandle, cost_model: &'a CostModel) -> Self {
        Self { flag, cost_model }
    }

    /// Evaluate one opportunity's [`CostInputs`] against the live flag + cost-gate.
    pub fn evaluate(&self, inputs: &CostInputs) -> PreSignDecision {
        evaluate_pre_sign(self.flag, self.cost_model, inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::econ::EconParams;
    use crate::metrics::health::{HealthEvaluator, KillSwitchSignal, Thresholds, TripReason};
    use crate::metrics::pnl::{PnlLedger, TxOutcome};
    use crate::metrics::registry::MetricsRegistry;
    use crate::signer::alert::LogSink;
    use crate::signer::killswitch::apply_health_signal;
    use solana_pubkey::Pubkey;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn healthy_inputs() -> CostInputs {
        CostInputs {
            spread_lamports: 500_000,
            swap_fees_lamports: 5_000,
            flash_fee_lamports: 0,
            tip_lamports: 50_000,
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 0.8,
        }
    }

    /// done-when (1): a fake signer is NEVER touched when the cost-gate vetoes an EV-negative
    /// opportunity — the veto happens via `CostModel::gate` before signing.
    #[test]
    fn ev_negative_is_cost_rejected_before_signing() {
        let flag = KillSwitchHandle::new();
        let model = CostModel::new(EconParams::default());
        let gate = PreSignGate::new(&flag, &model);

        // A fake signer that records every time it would be invoked.
        let signs = AtomicU64::new(0);
        let fake_sign = |_inputs: &CostInputs| {
            signs.fetch_add(1, Ordering::SeqCst);
        };

        // Spread far below costs => negative EV.
        let mut bad = healthy_inputs();
        bad.spread_lamports = 1_000;

        let decision = gate.evaluate(&bad);
        if decision.may_sign() {
            fake_sign(&bad);
        }
        assert!(matches!(
            decision,
            PreSignDecision::CostRejected {
                reason: RejectReason::NegativeExpectedValue,
                ..
            }
        ));
        assert_eq!(signs.load(Ordering::SeqCst), 0, "key must not be touched");

        // Sanity: a healthy opportunity DOES proceed (would sign).
        let ok = gate.evaluate(&healthy_inputs());
        assert!(ok.may_sign(), "{ok:?}");
    }

    /// done-when (2): a simulated revert-rate spike produces `Trip`, the supervisor consumes it and
    /// flips `signing-enabled=false`, after which the pre-sign gate returns `Halted` even for an
    /// otherwise-profitable opportunity.
    #[test]
    fn revert_rate_spike_trips_supervisor_and_halts_pre_sign() {
        let flag = KillSwitchHandle::new();
        let model = CostModel::new(EconParams::default());
        let gate = PreSignGate::new(&flag, &model);

        // Healthy first: a good opportunity may sign while the flag is up.
        assert!(gate.evaluate(&healthy_inputs()).may_sign());

        // Metrics side: build a >30% revert-rate window (10 lands + 15 reverts of 25 >= 20 min).
        let pnl = PnlLedger::new();
        let reg = MetricsRegistry::new();
        let tok = Pubkey::new_from_array([7; 32]);
        for _ in 0..10 {
            pnl.record_outcome(TxOutcome::landed(tok, 10, 0, 0, 0, 1_000))
                .unwrap();
        }
        for _ in 0..15 {
            pnl.record_outcome(TxOutcome::reverted_onchain(tok, 1, 1, 1_000))
                .unwrap();
        }
        let ev = HealthEvaluator::new(Thresholds::default());
        let signal = ev.evaluate(&pnl, &reg, 1_000, 1_000, 1_000);
        assert!(
            matches!(
                signal,
                KillSwitchSignal::Trip {
                    reason: TripReason::RevertRateExceeded { .. }
                }
            ),
            "{signal:?}"
        );

        // Signer side: the supervisor consumes the signal and owns the flag flip.
        let halted = apply_health_signal(&flag, signal, &LogSink);
        assert!(halted.is_some());
        assert!(!flag.signing_enabled());

        // Now even a strongly-profitable opportunity is gated by the flag, not the economics.
        assert_eq!(gate.evaluate(&healthy_inputs()), PreSignDecision::Halted);
    }

    #[test]
    fn flag_is_checked_before_the_cost_gate() {
        // A halted signer short-circuits to Halted without an economic verdict, even for a trade
        // that WOULD be cost-rejected — proving order (flag first).
        let flag = KillSwitchHandle::new();
        flag.halt(crate::signer::killswitch::HaltReason::Manual {
            operator: "op".into(),
        });
        let model = CostModel::new(EconParams::default());
        let mut bad = healthy_inputs();
        bad.spread_lamports = 1_000; // would be NegativeExpectedValue if reached
        assert_eq!(
            evaluate_pre_sign(&flag, &model, &bad),
            PreSignDecision::Halted
        );
    }
}
