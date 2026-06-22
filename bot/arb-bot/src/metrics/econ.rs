//! observ-4 / observ-5 — probabilistic unit-economics cost-gate + per-route `p_land` estimator.
//!
//! [`CostModel::e_net`] is the §10 expected-value formula, evaluated in deterministic `i128`
//! fixed-point (p_land quantized to parts-per-million) so it is allocation-free and bit-identical
//! wherever it runs — the signer can call [`CostModel::gate`] synchronously on the pre-sign path.
//!
//! ```text
//! E[net] = p_land·(spread − swap_fees − flash_fee − tip − prio − base)
//!          − (1 − p_land)·(prio + base) − rent_churn − E[rug/honeypot]
//! ```
//!
//! [`PLandEstimator`] supplies `p_land`: an EWMA of landing probability bucketed by
//! `(RouteKey, TipBucket)`, seeded with a conservative prior until `min_samples` outcomes arrive.

use std::collections::HashMap;
use std::sync::RwLock;

use super::types::RouteKey;

const PPM: i128 = 1_000_000;

/// All economic terms for one opportunity, in lamports (plus the landing probability).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostInputs {
    pub spread_lamports: u64,
    pub swap_fees_lamports: u64,
    pub flash_fee_lamports: u64,
    pub tip_lamports: u64,
    pub prio_lamports: u64,
    pub base_lamports: u64,
    /// Landing probability in `[0, 1]`.
    pub p_land: f64,
}

/// Static economic parameters (loaded from config).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EconParams {
    /// Amortized ALT/ATA open-close rent churn per opportunity.
    pub rent_churn_lamports: u64,
    /// Expected rug/honeypot loss term (fed by the SELL-sim gate, add-3).
    pub e_rug_honeypot_lamports: u64,
    /// Tip must not exceed this fraction of pre-tip gross profit (§9 "cap tip sebagai fraksi profit").
    pub tip_profit_fraction_cap: f64,
    /// Minimum E[net] to proceed (edge floor).
    pub min_edge_lamports: i128,
}

impl Default for EconParams {
    fn default() -> Self {
        Self {
            rent_churn_lamports: 0,
            e_rug_honeypot_lamports: 0,
            tip_profit_fraction_cap: 0.5,
            min_edge_lamports: 0,
        }
    }
}

/// Why the cost-gate rejected an opportunity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    NegativeExpectedValue,
    BelowMinEdge,
    TipExceedsProfitFraction,
}

/// Cost-gate decision; `i128` because p_land-weighted intermediates can be large/negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostGateDecision {
    Proceed {
        e_net_lamports: i128,
    },
    Reject {
        reason: RejectReason,
        e_net_lamports: i128,
    },
}

impl CostGateDecision {
    pub fn is_proceed(&self) -> bool {
        matches!(self, CostGateDecision::Proceed { .. })
    }
    pub fn e_net(&self) -> i128 {
        match self {
            CostGateDecision::Proceed { e_net_lamports } => *e_net_lamports,
            CostGateDecision::Reject { e_net_lamports, .. } => *e_net_lamports,
        }
    }
}

/// The probabilistic unit-economics model.
#[derive(Clone, Copy, Debug, Default)]
pub struct CostModel {
    pub params: EconParams,
}

impl CostModel {
    pub fn new(params: EconParams) -> Self {
        Self { params }
    }

    #[inline]
    fn p_ppm(p_land: f64) -> i128 {
        (p_land.clamp(0.0, 1.0) * PPM as f64).round() as i128
    }

    /// Pre-tip gross profit available to pay a tip: `spread − swap_fees − flash_fee − prio − base`.
    #[inline]
    fn gross_profit_for_tip(i: &CostInputs) -> i128 {
        i.spread_lamports as i128
            - i.swap_fees_lamports as i128
            - i.flash_fee_lamports as i128
            - i.prio_lamports as i128
            - i.base_lamports as i128
    }

    /// Expected net value in lamports (deterministic i128). See module formula.
    pub fn e_net(&self, i: &CostInputs) -> i128 {
        let p = Self::p_ppm(i.p_land);
        let win = i.spread_lamports as i128
            - i.swap_fees_lamports as i128
            - i.flash_fee_lamports as i128
            - i.tip_lamports as i128
            - i.prio_lamports as i128
            - i.base_lamports as i128;
        let lose = i.prio_lamports as i128 + i.base_lamports as i128;
        (p * win) / PPM
            - ((PPM - p) * lose) / PPM
            - self.params.rent_churn_lamports as i128
            - self.params.e_rug_honeypot_lamports as i128
    }

    /// Synchronous, allocation-free cost-gate. Reject order: negative-EV, below-min-edge, then the
    /// tip-fraction cap (a tip-cap can only veto an otherwise-passing trade).
    pub fn gate(&self, i: &CostInputs) -> CostGateDecision {
        let e_net = self.e_net(i);
        if e_net < 0 {
            return CostGateDecision::Reject {
                reason: RejectReason::NegativeExpectedValue,
                e_net_lamports: e_net,
            };
        }
        if e_net < self.params.min_edge_lamports {
            return CostGateDecision::Reject {
                reason: RejectReason::BelowMinEdge,
                e_net_lamports: e_net,
            };
        }
        // tip <= cap · gross_profit, compared in integer ppm to stay deterministic.
        let cap_ppm =
            (self.params.tip_profit_fraction_cap.clamp(0.0, 1.0) * PPM as f64).round() as i128;
        let gross = Self::gross_profit_for_tip(i);
        let tip_scaled = i.tip_lamports as i128 * PPM;
        let cap_scaled = cap_ppm * gross.max(0);
        if tip_scaled > cap_scaled {
            return CostGateDecision::Reject {
                reason: RejectReason::TipExceedsProfitFraction,
                e_net_lamports: e_net,
            };
        }
        CostGateDecision::Proceed {
            e_net_lamports: e_net,
        }
    }
}

/// Coarse tip bucket so `p_land` is estimated per competition tier, not per exact lamport value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TipBucket {
    /// < 10k lamports.
    VeryLow,
    /// 10k–100k.
    Low,
    /// 100k–1M.
    Mid,
    /// 1M–10M.
    High,
    /// ≥ 10M.
    VeryHigh,
}

impl TipBucket {
    pub fn from_lamports(tip: u64) -> Self {
        match tip {
            0..=9_999 => TipBucket::VeryLow,
            10_000..=99_999 => TipBucket::Low,
            100_000..=999_999 => TipBucket::Mid,
            1_000_000..=9_999_999 => TipBucket::High,
            _ => TipBucket::VeryHigh,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct EwmaState {
    p: f64,
    n: u64,
}

/// EWMA landing-probability estimator, bucketed by `(RouteKey, TipBucket)`.
#[derive(Debug)]
pub struct PLandEstimator {
    prior: f64,
    alpha: f64,
    min_samples: u64,
    buckets: RwLock<HashMap<(RouteKey, TipBucket), EwmaState>>,
}

impl PLandEstimator {
    /// `prior` is the conservative seed; `alpha` the EWMA smoothing (0,1]; `min_samples` the count
    /// below which `estimate` still returns the prior.
    pub fn new(prior: f64, alpha: f64, min_samples: u64) -> Self {
        Self {
            prior: prior.clamp(0.0, 1.0),
            alpha: alpha.clamp(f64::MIN_POSITIVE, 1.0),
            min_samples,
            buckets: RwLock::new(HashMap::new()),
        }
    }

    /// Landing probability for `(route, bucket)` — prior until `min_samples`, then the EWMA.
    pub fn estimate(&self, route: RouteKey, bucket: TipBucket) -> f64 {
        let map = self.buckets.read().unwrap();
        match map.get(&(route, bucket)) {
            Some(s) if s.n >= self.min_samples => s.p,
            _ => self.prior,
        }
    }

    /// Feed a landing outcome (`landed`) back into the `(route, bucket)` EWMA.
    pub fn update(&self, route: RouteKey, bucket: TipBucket, landed: bool) {
        let x = if landed { 1.0 } else { 0.0 };
        let mut map = self.buckets.write().unwrap();
        let s = map.entry((route, bucket)).or_insert(EwmaState {
            p: self.prior,
            n: 0,
        });
        if s.n == 0 {
            s.p = x;
        } else {
            s.p = self.alpha * x + (1.0 - self.alpha) * s.p;
        }
        s.n += 1;
    }

    /// Observed sample count for a `(route, bucket)`.
    pub fn samples(&self, route: RouteKey, bucket: TipBucket) -> u64 {
        self.buckets
            .read()
            .unwrap()
            .get(&(route, bucket))
            .map(|s| s.n)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::SwapDir;
    use solana_pubkey::Pubkey;

    fn route(a: u8, b: u8) -> RouteKey {
        RouteKey::new(
            Pubkey::new_from_array([a; 32]),
            Pubkey::new_from_array([b; 32]),
            SwapDir::AtoB,
        )
    }

    #[test]
    fn e_net_reproduces_formula_term_for_term() {
        let m = CostModel::new(EconParams {
            rent_churn_lamports: 1_000,
            e_rug_honeypot_lamports: 500,
            ..EconParams::default()
        });
        let i = CostInputs {
            spread_lamports: 100_000,
            swap_fees_lamports: 10_000,
            flash_fee_lamports: 0,
            tip_lamports: 20_000,
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 0.5,
        };
        // win = 100k - 10k - 0 - 20k - 5k - 5k = 60_000 ; lose = 10_000
        // e_net = 0.5*60_000 - 0.5*10_000 - 1_000 - 500 = 30_000 - 5_000 - 1_500 = 23_500
        assert_eq!(m.e_net(&i), 23_500);
    }

    #[test]
    fn gate_rejects_negative_ev_liquid_pair_with_tip_leakage() {
        // ~$1.58 avg profit, heavy tip + loser burn => negative EV. cap generous so the reason is
        // NegativeExpectedValue (not the tip-fraction cap).
        let m = CostModel::new(EconParams {
            rent_churn_lamports: 2_000,
            e_rug_honeypot_lamports: 0,
            tip_profit_fraction_cap: 0.9,
            min_edge_lamports: 0,
        });
        let i = CostInputs {
            spread_lamports: 20_000,
            swap_fees_lamports: 4_000,
            flash_fee_lamports: 0,
            tip_lamports: 9_000, // ~60% of the ~15k pre-tip gross
            prio_lamports: 3_000,
            base_lamports: 5_000,
            p_land: 0.25,
        };
        let d = m.gate(&i);
        assert!(!d.is_proceed());
        assert!(matches!(
            d,
            CostGateDecision::Reject {
                reason: RejectReason::NegativeExpectedValue,
                ..
            }
        ));
    }

    #[test]
    fn gate_rejects_tip_over_fraction_cap() {
        let m = CostModel::new(EconParams {
            tip_profit_fraction_cap: 0.5,
            ..EconParams::default()
        });
        // Strongly positive EV but the tip exceeds 50% of pre-tip gross.
        let i = CostInputs {
            spread_lamports: 1_000_000,
            swap_fees_lamports: 0,
            flash_fee_lamports: 0,
            tip_lamports: 600_000, // > 0.5 * (1_000_000 - 10_000) gross
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 0.95,
        };
        assert!(matches!(
            m.gate(&i),
            CostGateDecision::Reject {
                reason: RejectReason::TipExceedsProfitFraction,
                ..
            }
        ));
    }

    #[test]
    fn gate_proceeds_on_healthy_edge() {
        let m = CostModel::new(EconParams {
            tip_profit_fraction_cap: 0.7,
            min_edge_lamports: 1_000,
            ..EconParams::default()
        });
        let i = CostInputs {
            spread_lamports: 500_000,
            swap_fees_lamports: 5_000,
            flash_fee_lamports: 0,
            tip_lamports: 50_000,
            prio_lamports: 5_000,
            base_lamports: 5_000,
            p_land: 0.8,
        };
        let d = m.gate(&i);
        assert!(d.is_proceed(), "{d:?}");
        assert!(d.e_net() >= 1_000);
    }

    #[test]
    fn p_land_returns_prior_until_min_samples_then_ewma() {
        let est = PLandEstimator::new(0.2, 0.3, 5);
        let r = route(1, 2);
        let b = TipBucket::Mid;
        assert_eq!(est.estimate(r, b), 0.2); // prior
        for _ in 0..4 {
            est.update(r, b, true);
        }
        assert_eq!(est.estimate(r, b), 0.2); // still < min_samples
        est.update(r, b, true); // 5th
        assert!(est.estimate(r, b) > 0.2); // now EWMA, trending toward 1.0
        assert_eq!(est.samples(r, b), 5);
    }

    #[test]
    fn p_land_converges_to_true_rate() {
        let est = PLandEstimator::new(0.5, 0.05, 1);
        let r = route(3, 4);
        let b = TipBucket::High;
        // Deterministic, well-interleaved 70%-land stream (Bresenham-style, isolated misses) so
        // the EWMA converges tightly rather than oscillating with a periodic block pattern.
        for i in 0..2_000u32 {
            est.update(r, b, (i * 7) % 10 < 7);
        }
        let p = est.estimate(r, b);
        assert!((p - 0.7).abs() < 0.05, "p={p}");
    }

    #[test]
    fn distinct_tip_buckets_are_independent() {
        let est = PLandEstimator::new(0.5, 0.5, 1);
        let r = route(5, 6);
        est.update(r, TipBucket::Low, false);
        est.update(r, TipBucket::VeryHigh, true);
        assert!(est.estimate(r, TipBucket::Low) < 0.5);
        assert!(est.estimate(r, TipBucket::VeryHigh) > 0.5);
        assert_eq!(TipBucket::from_lamports(50_000), TipBucket::Low);
        assert_eq!(TipBucket::from_lamports(5_000_000), TipBucket::High);
    }
}
