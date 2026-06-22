//! landing-3 — TipOracle: size the Jito tip from the live tip-floor band, capped as a fraction of
//! simulated profit, and load-balance across the 8 runtime-resolved tip accounts.
//!
//! The tip sits in the `[p50, p75]` band, lerped toward p75 as `competition` rises, then clamped to
//! `profit_cap_frac · sim_profit` and floored at `MIN_TIP_LAMPORTS`. If the cap is below the floor
//! the opportunity cannot be tipped without exceeding the profit fraction, so `size_tip` returns
//! `None` (the caller's cost-gate rejects it). A stale tip-floor (older than `max_age_millis`)
//! triggers a conservative fallback: bid the minimum rather than chase an unknown auction.
//!
//! NOTE on the cap basis: this oracle caps against GROSS `sim_profit` (a fast pre-filter), whereas
//! `CostModel::gate` caps against NET pre-tip profit (`spread − fees − prio − base`). The executor
//! sizes the tip FIRST and re-runs the cost-gate on the sized tip ([`super::facade::land`]), so the
//! NET-basis gate is the BINDING check — the oracle's gross cap can only be looser, never the final
//! word.

use std::sync::atomic::{AtomicUsize, Ordering};

use solana_pubkey::Pubkey;

use super::types::{TipDecision, TipFloorSnapshot};

/// Jito minimum tip (plan §6).
pub const MIN_TIP_LAMPORTS: u64 = 1_000;

/// TipOracle sizing parameters.
#[derive(Clone, Copy, Debug)]
pub struct TipParams {
    pub percentile_low: f64,
    pub percentile_high: f64,
    pub profit_cap_frac: f64,
    pub max_age_millis: u64,
}

impl Default for TipParams {
    fn default() -> Self {
        Self {
            percentile_low: 0.50,
            percentile_high: 0.75,
            profit_cap_frac: 0.5,
            max_age_millis: 5_000,
        }
    }
}

/// Maintains the live tip-floor + the round-robin over the 8 tip accounts.
pub struct TipOracle {
    params: TipParams,
    snapshot: Option<TipFloorSnapshot>,
    tip_accounts: Vec<Pubkey>,
    rr: AtomicUsize,
}

/// Linear interpolation between the band percentiles given `t in [0,1]`.
fn lerp_band(snap: &TipFloorSnapshot, low: f64, high: f64, t: f64) -> u64 {
    // Map band edges onto p50/p75 (the band the plan targets).
    let lo = snap.p50 as f64 + (snap.p75 as f64 - snap.p50 as f64) * (low - 0.50) / 0.25;
    let hi = snap.p50 as f64 + (snap.p75 as f64 - snap.p50 as f64) * (high - 0.50) / 0.25;
    (lo + (hi - lo) * t.clamp(0.0, 1.0)).round() as u64
}

impl TipOracle {
    pub fn new(params: TipParams, tip_accounts: Vec<Pubkey>) -> Self {
        Self {
            params,
            snapshot: None,
            tip_accounts,
            rr: AtomicUsize::new(0),
        }
    }

    /// Push a fresh tip-floor (from REST poll or the WS stream).
    pub fn update_floor(&mut self, snapshot: TipFloorSnapshot) {
        self.snapshot = Some(snapshot);
    }

    /// Round-robin the next tip account (spreads auction load across all 8).
    pub fn next_tip_account(&self) -> Pubkey {
        let i = self.rr.fetch_add(1, Ordering::Relaxed) % self.tip_accounts.len();
        self.tip_accounts[i]
    }

    /// Size the tip. `competition in [0,1]` lerps the baseline from p50 toward p75. Returns `None`
    /// when the profit cap is below the Jito minimum (unviable — do not tip).
    pub fn size_tip(
        &self,
        sim_profit_lamports: u64,
        competition: f64,
        now_millis: u64,
    ) -> Option<TipDecision> {
        let cap = (self.params.profit_cap_frac * sim_profit_lamports as f64) as u64;
        if cap < MIN_TIP_LAMPORTS {
            return None; // cannot tip within the profit-fraction cap
        }

        let account = self.next_tip_account();

        // Stale or missing snapshot => conservative minimum bid.
        let baseline = match &self.snapshot {
            Some(s) if now_millis.saturating_sub(s.at_millis) <= self.params.max_age_millis => {
                lerp_band(
                    s,
                    self.params.percentile_low,
                    self.params.percentile_high,
                    competition,
                )
            }
            _ => MIN_TIP_LAMPORTS,
        };

        let capped_by_profit = baseline > cap;
        let lamports = baseline.min(cap).max(MIN_TIP_LAMPORTS);
        let percentile_used = (self.params.percentile_low
            + (self.params.percentile_high - self.params.percentile_low)
                * competition.clamp(0.0, 1.0))
            * 100.0;

        Some(TipDecision {
            lamports,
            percentile_used: percentile_used.round() as u32,
            capped_by_profit,
            account,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts() -> Vec<Pubkey> {
        (0..8u8).map(|i| Pubkey::new_from_array([i; 32])).collect()
    }

    fn snap(now: u64) -> TipFloorSnapshot {
        TipFloorSnapshot {
            p25: 1_000,
            p50: 10_000,
            p75: 20_000,
            p95: 50_000,
            p99: 100_000,
            ema: 12_000,
            at_millis: now,
        }
    }

    #[test]
    fn tip_stays_in_band_and_under_profit_cap() {
        let mut o = TipOracle::new(TipParams::default(), accounts());
        o.update_floor(snap(1_000));
        // Large profit => cap doesn't bind; tip in [p50, p75] = [10k, 20k].
        let d = o.size_tip(10_000_000, 0.0, 1_000).unwrap();
        assert_eq!(d.lamports, 10_000); // competition 0 => p50
        let d2 = o.size_tip(10_000_000, 1.0, 1_000).unwrap();
        assert_eq!(d2.lamports, 20_000); // competition 1 => p75
        assert!(!d.capped_by_profit);
    }

    #[test]
    fn tip_never_exceeds_profit_cap_frac() {
        let mut o = TipOracle::new(TipParams::default(), accounts());
        o.update_floor(snap(1_000));
        // profit 30_000, cap_frac 0.5 => cap 15_000. p75 baseline 20_000 > cap => clamp to 15_000.
        let d = o.size_tip(30_000, 1.0, 1_000).unwrap();
        assert_eq!(d.lamports, 15_000);
        assert!(d.capped_by_profit);
        assert!(d.lamports <= (0.5 * 30_000.0) as u64);
    }

    #[test]
    fn tip_never_below_minimum() {
        let mut o = TipOracle::new(TipParams::default(), accounts());
        o.update_floor(snap(1_000));
        // profit 4_000 => cap 2_000 (>= min 1_000); baseline floored to >= 1_000.
        let d = o.size_tip(4_000, 0.0, 1_000).unwrap();
        assert!(d.lamports >= MIN_TIP_LAMPORTS);
    }

    #[test]
    fn unviable_when_cap_below_minimum() {
        let o = TipOracle::new(TipParams::default(), accounts());
        // profit 1_000 => cap 500 < MIN_TIP_LAMPORTS => None.
        assert!(o.size_tip(1_000, 0.5, 1_000).is_none());
    }

    #[test]
    fn stale_floor_falls_back_to_minimum() {
        let mut o = TipOracle::new(TipParams::default(), accounts());
        o.update_floor(snap(0));
        // now far past max_age => conservative min bid.
        let d = o.size_tip(10_000_000, 1.0, 1_000_000).unwrap();
        assert_eq!(d.lamports, MIN_TIP_LAMPORTS);
    }

    #[test]
    fn load_balancer_exercises_all_eight_accounts() {
        let o = TipOracle::new(TipParams::default(), accounts());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            seen.insert(o.next_tip_account());
        }
        assert_eq!(seen.len(), 8);
    }
}
