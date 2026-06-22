//! landing-8 — the `Executor::land` facade: orchestrate the pre-sign gates and the landing loop.
//!
//! Order (plan §6 / §9): kill-switch check → probabilistic cost-gate → writable-account dedupe
//! (add-1) → tip sizing → landing loop → record outcome into the metrics pipeline. The signer's
//! `signing-enabled` flag is checked before any work, and the routing-exclusivity invariant is
//! enforced when a fallback route is chosen. The actual sign + network submit live behind the
//! [`SignerHandle`] / [`LandingTransport`] seams.

use solana_pubkey::Pubkey;

use crate::metrics::econ::{CostInputs, CostModel, RejectReason};
use crate::metrics::registry::MetricsRegistry;
use crate::metrics::types::RevertCause;

use super::config::ExecutorConfig;
use super::landing_loop::{run_landing_loop, BlockhashSource, LandingTransport};
use super::registry::WritableAccountRegistry;
use super::tip::TipOracle;
use super::types::{ArbTxSpec, DropCause, LandingOutcome, Route};

/// The signer side the executor depends on (does NOT implement). Mirrors the signer sidecar's
/// outflow-gate surface without coupling the executor to its internals.
pub trait SignerHandle {
    fn signing_enabled(&self) -> bool;
    fn payer(&self) -> Pubkey;
}

/// One land request.
#[derive(Clone, Debug)]
pub struct LandRequest {
    pub spec: ArbTxSpec,
    /// Economic terms for the synchronous cost-gate.
    pub cost_inputs: CostInputs,
    /// Auction competition in `[0,1]` (lerps the tip toward p75).
    pub competition: f64,
    pub now_millis: u64,
}

/// Why `land` refused before ever submitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandError {
    /// Kill-switch engaged.
    Halted,
    /// Cost-gate rejected the opportunity.
    CostGateRejected(RejectReason),
    /// add-1: another opportunity already holds a writable lock on one of the route pools.
    WritableContention,
    /// Tip cannot be sized within the profit-fraction cap.
    TipUnviable,
    /// A jitodontfront tx was about to leave via a non-Jito route (routing-exclusivity).
    RoutingExclusivityViolation,
}

/// All the seams `land` needs.
pub struct LandDeps<'a> {
    pub signer: &'a dyn SignerHandle,
    pub source: &'a dyn BlockhashSource,
    pub transport: &'a dyn LandingTransport,
    pub tip_oracle: &'a TipOracle,
    pub cost_model: &'a CostModel,
    pub registry: &'a WritableAccountRegistry,
    pub metrics: &'a MetricsRegistry,
    pub config: &'a ExecutorConfig,
}

fn drop_cause_to_revert_cause(c: DropCause) -> RevertCause {
    match c {
        DropCause::TipAuctionLost => RevertCause::TipLost,
        DropCause::Congestion | DropCause::TooLateInSlot | DropCause::RateLimited => {
            RevertCause::Congestion
        }
        DropCause::StaleBlockhash | DropCause::UncledOrSkipped => RevertCause::StaleBlockhash,
        DropCause::SimFailed => RevertCause::SimFail,
        DropCause::SenderRejected | DropCause::WritableContention | DropCause::Unknown => {
            RevertCause::Unknown
        }
    }
}

/// The default route is the first configured Jito region; a jitodontfront tx may only use a
/// Jito-protected route (routing-exclusivity). `has_jitodontfront` reflects the assembled tx.
fn select_route(
    config: &ExecutorConfig,
    has_jitodontfront: bool,
    route: Route,
) -> Result<Route, LandError> {
    let _ = config;
    if has_jitodontfront && !route.is_jito_protected() {
        return Err(LandError::RoutingExclusivityViolation);
    }
    Ok(route)
}

/// Orchestrate one landing attempt sequence.
pub fn land(deps: &LandDeps, req: &LandRequest) -> Result<LandingOutcome, LandError> {
    // 1. Kill-switch.
    if !deps.signer.signing_enabled() {
        return Err(LandError::Halted);
    }

    // 2. Tip sizing FIRST — the cost-gate must score the tip that ACTUALLY rides in the tx, not a
    // caller-supplied placeholder (dec-3: one shared tip definition across gate/builder/assert).
    let tip = deps
        .tip_oracle
        .size_tip(
            req.spec.sim_profit_lamports,
            req.competition,
            req.now_millis,
        )
        .ok_or(LandError::TipUnviable)?;

    // 3. Probabilistic cost-gate (synchronous) on the REAL tip.
    let mut cost_inputs = req.cost_inputs;
    cost_inputs.tip_lamports = tip.lamports;
    if let crate::metrics::econ::CostGateDecision::Reject { reason, .. } =
        deps.cost_model.gate(&cost_inputs)
    {
        return Err(LandError::CostGateRejected(reason));
    }

    // 4. add-1 — one-inflight-per-pool. Held until the end of this attempt sequence.
    let _guard = deps
        .registry
        .try_acquire(&req.spec.route_pools)
        .map_err(|_| LandError::WritableContention)?;

    // 5. Route selection (default Jito bundle, nearest region). A real jitodontfront tx carries the
    // marker; here the assembled bundle always carries it (routing-exclusivity honored).
    let default_route = Route::JitoBundle {
        region: *deps
            .config
            .region_order
            .first()
            .unwrap_or(&super::types::Region::Frankfurt),
    };
    let route = select_route(deps.config, true, default_route)?;

    // 6. Landing loop.
    deps.metrics.record_attempt();
    let outcome = run_landing_loop(
        deps.source,
        deps.transport,
        &req.spec,
        route,
        tip.lamports,
        deps.config.max_attempts,
    );

    // 7. Record outcome into the metrics pipeline.
    match &outcome {
        LandingOutcome::Landed {
            slot,
            tip_paid_lamports,
            ..
        } => {
            // Realized profit ≈ sim profit minus the tip actually paid (refined post-land by the
            // bit-exact slippage mirror).
            let profit = req
                .spec
                .sim_profit_lamports
                .saturating_sub(*tip_paid_lamports);
            deps.metrics.record_land(profit, *slot, 0);
        }
        LandingOutcome::Reverted {
            burned_lamports, ..
        } => {
            deps.metrics
                .record_revert(RevertCause::OnchainUnprofitable, *burned_lamports);
        }
        LandingOutcome::GaveUp { last_cause, .. } => {
            deps.metrics
                .record_revert(drop_cause_to_revert_cause(*last_cause), 0);
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::landing_loop::AttemptResult;
    use crate::executor::tip::TipParams;
    use crate::executor::types::{Blockhash, TipFloorSnapshot};
    use crate::metrics::econ::EconParams;
    use std::sync::atomic::{AtomicU8, Ordering};

    struct EnabledSigner(bool, Pubkey);
    impl SignerHandle for EnabledSigner {
        fn signing_enabled(&self) -> bool {
            self.0
        }
        fn payer(&self) -> Pubkey {
            self.1
        }
    }

    #[derive(Default)]
    struct Src(AtomicU8);
    impl BlockhashSource for Src {
        fn fresh(&self) -> Result<Blockhash, DropCause> {
            Ok(Blockhash([self.0.fetch_add(1, Ordering::Relaxed); 32]))
        }
    }

    struct LandFirst;
    impl LandingTransport for LandFirst {
        fn attempt(&self, _s: &ArbTxSpec, _b: Blockhash, attempt: u8) -> AttemptResult {
            AttemptResult::Landed {
                slot: 500 + attempt as u64,
            }
        }
    }

    fn oracle() -> TipOracle {
        let mut o = TipOracle::new(
            TipParams::default(),
            (0..8u8).map(|i| Pubkey::new_from_array([i; 32])).collect(),
        );
        o.update_floor(TipFloorSnapshot {
            p25: 1_000,
            p50: 10_000,
            p75: 20_000,
            p95: 50_000,
            p99: 100_000,
            ema: 12_000,
            at_millis: 0,
        });
        o
    }

    fn good_inputs() -> CostInputs {
        CostInputs {
            spread_lamports: 500_000,
            swap_fees_lamports: 5_000,
            flash_fee_lamports: 0,
            tip_lamports: 20_000,
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 0.8,
        }
    }

    fn request(pools: Vec<Pubkey>) -> LandRequest {
        LandRequest {
            spec: ArbTxSpec {
                payer: Pubkey::new_from_array([1; 32]),
                cu_limit: 200_000,
                cu_price_micro: 50,
                sim_profit_lamports: 500_000,
                route_pools: pools,
                alt_tables: vec![],
            },
            cost_inputs: good_inputs(),
            competition: 0.0,
            now_millis: 0,
        }
    }

    #[allow(clippy::too_many_arguments)] // test-only assembly of the 8 borrowed seams
    fn deps<'a>(
        signer: &'a EnabledSigner,
        src: &'a Src,
        transport: &'a dyn LandingTransport,
        oracle: &'a TipOracle,
        cost: &'a CostModel,
        reg: &'a WritableAccountRegistry,
        metrics: &'a MetricsRegistry,
        config: &'a ExecutorConfig,
    ) -> LandDeps<'a> {
        LandDeps {
            signer,
            source: src,
            transport,
            tip_oracle: oracle,
            cost_model: cost,
            registry: reg,
            metrics,
            config,
        }
    }

    #[test]
    fn halted_signer_blocks_land() {
        let signer = EnabledSigner(false, Pubkey::new_from_array([1; 32]));
        let src = Src::default();
        let o = oracle();
        let cost = CostModel::new(EconParams::default());
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let d = deps(&signer, &src, &LandFirst, &o, &cost, &reg, &m, &cfg);
        assert_eq!(land(&d, &request(vec![])).unwrap_err(), LandError::Halted);
    }

    #[test]
    fn happy_path_lands_and_records_metric() {
        let signer = EnabledSigner(true, Pubkey::new_from_array([1; 32]));
        let src = Src::default();
        let o = oracle();
        let cost = CostModel::new(EconParams::default());
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let d = deps(&signer, &src, &LandFirst, &o, &cost, &reg, &m, &cfg);
        let outcome = land(&d, &request(vec![Pubkey::new_from_array([7; 32])])).unwrap();
        assert!(matches!(outcome, LandingOutcome::Landed { .. }));
        assert_eq!(m.snapshot().lands, 1);
        assert_eq!(m.snapshot().attempts, 1);
        // Pool lock released after the sequence.
        assert_eq!(reg.inflight_count(), 0);
    }

    #[test]
    fn negative_ev_is_cost_gated() {
        let signer = EnabledSigner(true, Pubkey::new_from_array([1; 32]));
        let src = Src::default();
        let o = oracle();
        let cost = CostModel::new(EconParams::default());
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let d = deps(&signer, &src, &LandFirst, &o, &cost, &reg, &m, &cfg);
        let mut req = request(vec![]);
        // Spread below costs => negative EV.
        req.cost_inputs.spread_lamports = 1_000;
        assert!(matches!(
            land(&d, &req).unwrap_err(),
            LandError::CostGateRejected(RejectReason::NegativeExpectedValue)
        ));
    }

    #[test]
    fn second_opportunity_on_same_pool_is_contended() {
        let signer = EnabledSigner(true, Pubkey::new_from_array([1; 32]));
        let o = oracle();
        let cost = CostModel::new(EconParams::default());
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let pool = Pubkey::new_from_array([7; 32]);
        // Hold a lock on the pool, then a land on the same pool is contended.
        let _g = reg.try_acquire(&[pool]).unwrap();
        let src = Src::default();
        let d = deps(&signer, &src, &LandFirst, &o, &cost, &reg, &m, &cfg);
        assert_eq!(
            land(&d, &request(vec![pool])).unwrap_err(),
            LandError::WritableContention
        );
    }

    #[test]
    fn gave_up_records_revert_with_cause() {
        struct NeverLands;
        impl LandingTransport for NeverLands {
            fn attempt(&self, _s: &ArbTxSpec, _b: Blockhash, _a: u8) -> AttemptResult {
                AttemptResult::NoLand {
                    cause: DropCause::Congestion,
                }
            }
        }
        let signer = EnabledSigner(true, Pubkey::new_from_array([1; 32]));
        let src = Src::default();
        let o = oracle();
        let cost = CostModel::new(EconParams::default());
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let d = deps(&signer, &src, &NeverLands, &o, &cost, &reg, &m, &cfg);
        let outcome = land(&d, &request(vec![])).unwrap();
        assert!(matches!(outcome, LandingOutcome::GaveUp { .. }));
        assert_eq!(m.snapshot().reverts, 1);
        assert_eq!(m.revert_cause_count(RevertCause::Congestion), 1);
    }

    #[test]
    fn cost_gate_scores_the_sized_tip_not_the_placeholder() {
        // dec-3 regression: a tiny placeholder tip would pass the gate, but the tip the oracle
        // actually sizes exceeds the gate's profit-fraction cap and must be vetoed.
        let signer = EnabledSigner(true, Pubkey::new_from_array([1; 32]));
        let src = Src::default();
        let o = oracle(); // p50=10_000, p75=20_000
        let cost = CostModel::new(EconParams {
            tip_profit_fraction_cap: 0.5,
            ..EconParams::default()
        });
        let reg = WritableAccountRegistry::new();
        let m = MetricsRegistry::new();
        let cfg = ExecutorConfig::default();
        let d = deps(&signer, &src, &LandFirst, &o, &cost, &reg, &m, &cfg);
        let req = LandRequest {
            spec: ArbTxSpec {
                payer: Pubkey::new_from_array([1; 32]),
                cu_limit: 200_000,
                cu_price_micro: 50,
                sim_profit_lamports: 100_000, // size cap = 50_000 => sized tip = p75 = 20_000
                route_pools: vec![],
                alt_tables: vec![],
            },
            cost_inputs: CostInputs {
                spread_lamports: 50_000,
                swap_fees_lamports: 10_000,
                flash_fee_lamports: 0,
                tip_lamports: 1_000, // placeholder the OLD gate would have happily passed
                prio_lamports: 5_000,
                base_lamports: 5_000,
                p_land: 0.8,
            },
            competition: 1.0, // => sized tip rides to p75 = 20_000
            now_millis: 0,
        };
        // gross_profit_for_tip = 50_000-10_000-5_000-5_000 = 30_000 => gate cap = 15_000.
        // sized tip 20_000 > 15_000 => TipExceedsProfitFraction (only visible because the gate now
        // sees the sized tip, not the 1_000 placeholder).
        assert!(matches!(
            land(&d, &req).unwrap_err(),
            LandError::CostGateRejected(RejectReason::TipExceedsProfitFraction)
        ));
    }
}
