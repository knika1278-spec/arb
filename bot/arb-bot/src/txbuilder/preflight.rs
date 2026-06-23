//! Preflight simulation wrapper + profit-from-balance-delta check (txbuilder-7).
//!
//! Two responsibilities:
//! 1. **profit-check from the ACTUAL balance delta** (invariant §7) — never the instruction
//!    `amount`. The base ATA's post-minus-pre delta is the real round-trip profit; Token-2022
//!    fees and rounding are already baked into it.
//! 2. A simulation seam (`SimulateRpc`) mirroring the detection module's `AccountUpdateSource`
//!    pattern: the real `simulateTransaction` client (needs `solana-client` w/ rustls) lands
//!    in the executor phase; here we define the contract + a host mock so the profit-gate is
//!    fully unit-tested now.
//!
//! This is a cheap pre-sign guard, NOT the safety net — the on-chain terminal assert is
//! authoritative even under `skipPreflight=true` (invariant §2).

use crate::txbuilder::error::TxBuilderError;
use solana_program::instruction::Instruction;
use solana_pubkey::Pubkey;

/// Realized base-asset profit = `base_post - base_pre` (may be negative).
pub fn profit_from_balances(base_pre: u64, base_post: u64) -> i128 {
    base_post as i128 - base_pre as i128
}

/// Gate the round-trip on the realized base-ATA delta meeting the costs-inclusive `min_profit`.
pub fn check_profit(base_pre: u64, base_post: u64, min_profit: u64) -> Result<u64, TxBuilderError> {
    let realized = profit_from_balances(base_pre, base_post);
    if realized < min_profit as i128 {
        return Err(TxBuilderError::BelowMinProfit {
            predicted: realized,
            min_profit,
        });
    }
    Ok(realized as u64)
}

/// What a simulation tells us about a candidate tx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimOutcome {
    /// CU actually consumed — feeds `ComputeBudgetParams::from_measured`.
    pub units_consumed: u32,
    /// Base ATA amount before / after the simulated round trip.
    pub base_pre: u64,
    pub base_post: u64,
    /// `Some(code)` if the program reverted (decode vs `arb_types::ArbError`).
    pub err_code: Option<u32>,
}

/// A successfully-simulated, profitable candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreflightOk {
    pub realized_profit: u64,
    pub units_consumed: u32,
}

/// Seam for `simulateTransaction`. The real RPC impl arrives with the executor; tests use
/// [`MockSimulator`].
pub trait SimulateRpc {
    fn simulate(
        &self,
        instructions: &[Instruction],
        signers: &[Pubkey],
    ) -> Result<SimOutcome, TxBuilderError>;
}

/// Evaluate a simulation outcome: reverted => error; profitable => `PreflightOk`.
pub fn evaluate(outcome: SimOutcome, min_profit: u64) -> Result<PreflightOk, TxBuilderError> {
    if let Some(code) = outcome.err_code {
        return Err(TxBuilderError::SimulationReverted { code: Some(code) });
    }
    let realized_profit = check_profit(outcome.base_pre, outcome.base_post, min_profit)?;
    Ok(PreflightOk {
        realized_profit,
        units_consumed: outcome.units_consumed,
    })
}

/// Decode a simulation revert code into the shared [`arb_types::ArbError`] when it is one of the
/// arb-program's own 6000-based codes. `None` ⇒ the custom code came from a venue CPI (not our
/// terminal assert), so the revert is a venue/route problem, not a profit/trust-boundary failure.
pub fn decode_revert(code: u32) -> Option<arb_types::ArbError> {
    arb_types::ArbError::from_code(code)
}

/// Preflight `simulateTransaction` wrapper (txbuilder-7): run the simulation over the [`SimulateRpc`]
/// seam, then [`evaluate`] the outcome in one call so a caller can never simulate-without-checking.
/// A revert (any custom code) and a below-`min_profit` realized delta both surface as a
/// [`TxBuilderError`]; on success the measured CU + realized profit come back for the next rebuild.
pub fn preflight_simulate(
    rpc: &dyn SimulateRpc,
    instructions: &[Instruction],
    signers: &[Pubkey],
    min_profit: u64,
) -> Result<PreflightOk, TxBuilderError> {
    let outcome = rpc.simulate(instructions, signers)?;
    evaluate(outcome, min_profit)
}

/// Host mock returning a canned outcome (test substrate until the RPC client lands).
pub struct MockSimulator {
    pub outcome: SimOutcome,
}

impl SimulateRpc for MockSimulator {
    fn simulate(
        &self,
        _instructions: &[Instruction],
        _signers: &[Pubkey],
    ) -> Result<SimOutcome, TxBuilderError> {
        Ok(self.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profit_delta_can_be_negative() {
        assert_eq!(profit_from_balances(1_000, 1_200), 200);
        assert_eq!(profit_from_balances(1_000, 900), -100);
    }

    #[test]
    fn check_profit_enforces_min() {
        assert_eq!(check_profit(1_000, 1_200, 150).unwrap(), 200);
        assert_eq!(
            check_profit(1_000, 1_050, 100).unwrap_err(),
            TxBuilderError::BelowMinProfit {
                predicted: 50,
                min_profit: 100
            }
        );
    }

    #[test]
    fn evaluate_rejects_revert_and_unprofitable() {
        let reverted = SimOutcome {
            units_consumed: 100_000,
            base_pre: 1_000,
            base_post: 1_000,
            err_code: Some(6000),
        };
        assert_eq!(
            evaluate(reverted, 1).unwrap_err(),
            TxBuilderError::SimulationReverted { code: Some(6000) }
        );

        let ok = SimOutcome {
            units_consumed: 120_000,
            base_pre: 1_000,
            base_post: 1_500,
            err_code: None,
        };
        let r = evaluate(ok, 300).unwrap();
        assert_eq!(r.realized_profit, 500);
        assert_eq!(r.units_consumed, 120_000);
    }

    #[test]
    fn mock_simulator_returns_canned_outcome() {
        let sim = MockSimulator {
            outcome: SimOutcome {
                units_consumed: 90_000,
                base_pre: 10,
                base_post: 25,
                err_code: None,
            },
        };
        let out = sim.simulate(&[], &[]).unwrap();
        assert_eq!(out.base_post, 25);
    }

    #[test]
    fn preflight_simulate_wraps_simulate_then_evaluate() {
        // Profitable mock => Ok with the realized delta + measured CU, in one call.
        let ok = MockSimulator {
            outcome: SimOutcome {
                units_consumed: 111_000,
                base_pre: 1_000,
                base_post: 1_800,
                err_code: None,
            },
        };
        let r = preflight_simulate(&ok, &[], &[], 500).unwrap();
        assert_eq!(r.realized_profit, 800);
        assert_eq!(r.units_consumed, 111_000);

        // Reverting mock => the wrapper surfaces SimulationReverted (never an Ok the caller could
        // forget to check).
        let reverted = MockSimulator {
            outcome: SimOutcome {
                units_consumed: 10_000,
                base_pre: 1_000,
                base_post: 1_000,
                err_code: Some(6010),
            },
        };
        assert_eq!(
            preflight_simulate(&reverted, &[], &[], 1).unwrap_err(),
            TxBuilderError::SimulationReverted { code: Some(6010) }
        );

        // Profit below the floor => BelowMinProfit.
        let thin = MockSimulator {
            outcome: SimOutcome {
                units_consumed: 90_000,
                base_pre: 1_000,
                base_post: 1_050,
                err_code: None,
            },
        };
        assert_eq!(
            preflight_simulate(&thin, &[], &[], 100).unwrap_err(),
            TxBuilderError::BelowMinProfit {
                predicted: 50,
                min_profit: 100
            }
        );
    }

    #[test]
    fn decode_revert_maps_arb_program_codes() {
        use arb_types::ArbError;
        assert_eq!(decode_revert(6000), Some(ArbError::Unprofitable));
        assert_eq!(decode_revert(6010), Some(ArbError::RouteDoesNotClose));
        // A custom code outside the arb-program range is a venue-CPI revert, not one of ours.
        assert_eq!(decode_revert(1), None);
    }
}
