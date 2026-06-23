//! landing-5 — the pre-tip simulation gate.
//!
//! Before the landing loop spends a single lamport — a tip on a tx that would revert, or
//! priority/base fee on a tx that lands-but-reverts — simulate the FULLY-ASSEMBLED atomic arb tx
//! (tip riding inside, invariant #10) and proceed only if it (a) does not revert and (b) clears the
//! costs-inclusive `min_profit` floor on the REALIZED base-ATA balance delta (invariant §7 — the
//! post-minus-pre delta, never the instruction `amount`). A failing simulation drops the
//! opportunity as [`DropCause::SimFailed`]; because the executor never submits, the tx burns nothing
//! (plan §2 "biaya nol"). This is a cheap guard, NOT the safety net — the on-chain terminal assert
//! stays authoritative even under `skipPreflight=true` (invariant §2).
//!
//! The sized Jito tip is threaded INTO the seam (out-of-band, exactly like the landing loop receives
//! `tip.lamports`) so the real transport assembles the precise tip-inside tx it will submit — without
//! it a transport could not honor invariant #10 and its simulated outcome would not reflect the real
//! tx. The profit is still gated against the costs-inclusive `min_profit` floor (dec-3), which is the
//! single place the tip is priced in.
//!
//! Two networked backends sit behind the single [`PreTipSimulator`] seam: a single-tx
//! `simulateTransaction` (Helius / RPC, mirrors [`crate::txbuilder::SimOutcome`]) and a Jito
//! `simulateBundle` ([`crate::executor::jito::JitoClient::simulate_bundle`]). The gate logic here is
//! backend-agnostic and fully host-tested; the real transports implement [`PreTipSimulator`] in
//! their phase (reqwest-rustls + solana-client), exactly like [`super::landing_loop::LandingTransport`].

use super::types::{ArbTxSpec, DropCause};

/// What a pre-tip simulation reports about the assembled arb tx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreTipSimResult {
    /// `Some(code)` if the simulated tx reverted (decode vs `arb_types::ArbError` via
    /// [`crate::txbuilder::decode_revert`]); `None` on a clean run.
    pub revert_code: Option<u32>,
    /// Realized base-asset profit from the simulated balance delta (`base_post - base_pre`); may be
    /// negative when the round-trip clears on-chain but comes back under water.
    pub realized_profit_lamports: i128,
    /// CU the simulation consumed. Carried for a caller that wants to refine
    /// `ComputeBudgetParams::from_measured` on a later rebuild; landing-5 itself is a pass/fail gate
    /// and does not consume it.
    pub units_consumed: u32,
}

/// A simulated tx that passed the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreTipOk {
    pub realized_profit_lamports: u64,
    pub units_consumed: u32,
}

/// Why the pre-tip gate refused. All map to [`DropCause::SimFailed`] except an `Unavailable`
/// transport error, which carries the transport's own cause through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreTipReject {
    /// The simulated tx reverted on-chain (program custom code when decodable).
    Reverted { code: Option<u32> },
    /// The round-trip cleared but the realized base delta is below the costs-inclusive floor.
    Unprofitable { realized: i128, min_profit: u64 },
    /// The simulation itself could not be performed (transport/RPC failure). Fail-closed: a gate
    /// that cannot run drops the opportunity rather than sending blind.
    Unavailable { cause: DropCause },
}

impl PreTipReject {
    /// The drop cause to record. A revert/unprofitable drop is a `SimFailed`; an `Unavailable`
    /// forwards the transport cause (e.g. `RateLimited`) so the metric attribution stays honest.
    pub fn drop_cause(self) -> DropCause {
        match self {
            PreTipReject::Unavailable { cause } => cause,
            _ => DropCause::SimFailed,
        }
    }
}

/// Pre-tip simulation seam. The real impl assembles `spec` WITH a tip transfer of `tip_lamports`
/// inside (invariant #10), runs `simulateTransaction` (Helius/RPC) or `simulateBundle` (Jito), and
/// returns the realized delta + CU; a transport failure returns the [`DropCause`] to attribute. The
/// sized tip is threaded out-of-band exactly as [`super::landing_loop::LandingTransport`] receives
/// it. Kept sync so the gate + its tests carry no async runtime.
pub trait PreTipSimulator {
    fn simulate(&self, spec: &ArbTxSpec, tip_lamports: u64) -> Result<PreTipSimResult, DropCause>;
}

/// Evaluate a simulation result against the costs-inclusive `min_profit` floor (the SAME value the
/// on-chain terminal assert enforces — dec-3): revert => reject, below-floor => reject, else proceed.
pub fn evaluate_pre_tip(
    result: PreTipSimResult,
    min_profit: u64,
) -> Result<PreTipOk, PreTipReject> {
    if let Some(code) = result.revert_code {
        return Err(PreTipReject::Reverted { code: Some(code) });
    }
    if result.realized_profit_lamports < min_profit as i128 {
        return Err(PreTipReject::Unprofitable {
            realized: result.realized_profit_lamports,
            min_profit,
        });
    }
    Ok(PreTipOk {
        realized_profit_lamports: result.realized_profit_lamports as u64,
        units_consumed: result.units_consumed,
    })
}

/// Run the gate end-to-end: simulate the assembled tx (tip inside) over the seam, then evaluate. A
/// transport failure is folded into [`PreTipReject::Unavailable`] (fail-closed).
pub fn run_pre_tip_gate(
    sim: &dyn PreTipSimulator,
    spec: &ArbTxSpec,
    tip_lamports: u64,
    min_profit: u64,
) -> Result<PreTipOk, PreTipReject> {
    match sim.simulate(spec, tip_lamports) {
        Ok(result) => evaluate_pre_tip(result, min_profit),
        Err(cause) => Err(PreTipReject::Unavailable { cause }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;

    fn spec() -> ArbTxSpec {
        ArbTxSpec {
            payer: Pubkey::new_from_array([1; 32]),
            cu_limit: 200_000,
            cu_price_micro: 50,
            sim_profit_lamports: 100_000,
            route_pools: vec![Pubkey::new_from_array([9; 32])],
            alt_tables: vec![],
        }
    }

    /// A simulator returning a canned result (or a transport error); records the tip it was handed.
    struct CannedSim {
        out: Result<PreTipSimResult, DropCause>,
        seen_tip: std::cell::Cell<u64>,
    }
    impl CannedSim {
        fn new(out: Result<PreTipSimResult, DropCause>) -> Self {
            Self {
                out,
                seen_tip: std::cell::Cell::new(u64::MAX),
            }
        }
    }
    impl PreTipSimulator for CannedSim {
        fn simulate(
            &self,
            _spec: &ArbTxSpec,
            tip_lamports: u64,
        ) -> Result<PreTipSimResult, DropCause> {
            self.seen_tip.set(tip_lamports);
            self.out
        }
    }

    #[test]
    fn profitable_sim_proceeds_with_realized_delta_and_cu() {
        let r = PreTipSimResult {
            revert_code: None,
            realized_profit_lamports: 12_345,
            units_consumed: 180_000,
        };
        let ok = evaluate_pre_tip(r, 5_000).unwrap();
        assert_eq!(ok.realized_profit_lamports, 12_345);
        assert_eq!(ok.units_consumed, 180_000);
    }

    #[test]
    fn reverting_sim_is_rejected_as_sim_failed() {
        let r = PreTipSimResult {
            revert_code: Some(6000), // Unprofitable on-chain assert
            realized_profit_lamports: 0,
            units_consumed: 9_000,
        };
        let rej = evaluate_pre_tip(r, 1).unwrap_err();
        assert_eq!(rej, PreTipReject::Reverted { code: Some(6000) });
        assert_eq!(rej.drop_cause(), DropCause::SimFailed);
    }

    #[test]
    fn below_floor_sim_is_rejected_even_when_it_lands() {
        // Clears on-chain (no revert) but the realized delta is under the costs-inclusive floor.
        let r = PreTipSimResult {
            revert_code: None,
            realized_profit_lamports: 400,
            units_consumed: 150_000,
        };
        let rej = evaluate_pre_tip(r, 1_000).unwrap_err();
        assert_eq!(
            rej,
            PreTipReject::Unprofitable {
                realized: 400,
                min_profit: 1_000
            }
        );
        assert_eq!(rej.drop_cause(), DropCause::SimFailed);
    }

    #[test]
    fn negative_realized_delta_is_unprofitable() {
        let r = PreTipSimResult {
            revert_code: None,
            realized_profit_lamports: -250,
            units_consumed: 150_000,
        };
        assert!(matches!(
            evaluate_pre_tip(r, 0).unwrap_err(),
            PreTipReject::Unprofitable { realized: -250, .. }
        ));
    }

    #[test]
    fn transport_failure_fails_closed_forwarding_cause() {
        let sim = CannedSim::new(Err(DropCause::RateLimited));
        let rej = run_pre_tip_gate(&sim, &spec(), 5_000, 0).unwrap_err();
        assert_eq!(
            rej,
            PreTipReject::Unavailable {
                cause: DropCause::RateLimited
            }
        );
        // Unavailable forwards the transport cause, NOT SimFailed.
        assert_eq!(rej.drop_cause(), DropCause::RateLimited);
    }

    #[test]
    fn run_gate_threads_tip_into_simulate_then_evaluates() {
        let sim = CannedSim::new(Ok(PreTipSimResult {
            revert_code: None,
            realized_profit_lamports: 50_000,
            units_consumed: 200_000,
        }));
        let ok = run_pre_tip_gate(&sim, &spec(), 12_345, 10_000).unwrap();
        assert_eq!(ok.realized_profit_lamports, 50_000);
        // The sized tip was threaded to the simulator so it can assemble the tip-inside tx (#10).
        assert_eq!(sim.seen_tip.get(), 12_345);
    }
}
