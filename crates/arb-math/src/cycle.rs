//! N-leg cycle composition + sizing (`sizing-15`): the triangle (3-leg) and general `N`-leg
//! analogue of the 2-leg [`crate::cpmm::RoundTrip`] + [`crate::search::optimal_delta_search`].
//!
//! A cycle is an ordered list of legs `base → t1 → … → t_{N-1} → base`, each leg a
//! [`Quoter`] + a [`SwapDir`]. Composition chains each leg's NET output (balance delta, after
//! Token-2022 transfer fees) into the next leg's input — exactly what the on-chain N-leg
//! processor (`onchain-20`) measures. This is the heterogeneous-venue path: any mix of venues
//! that implement [`Quoter`] (Raydium CPMM / PumpSwap today; the Fase-2.5 venues once wrapped)
//! composes here, so the triangle can span e.g. Meteora DAMM v2 ↔ DLMM (the ANB pattern).
//!
//! Cycle profit `final_out − delta_in` is unimodal in the input size for monotone-concave legs
//! (constant-product, in-range concentrated liquidity, single-bin constant-sum), so the same
//! exact-integer ternary search as `optimal_delta_search` finds the true optimum — no float.

use crate::venue::{QuoteIn, Quoter};
use arb_types::SwapDir;

/// One leg of a cycle: the venue quoter and the swap direction for this hop.
pub struct CycleLeg<'a> {
    pub quoter: &'a dyn Quoter,
    pub dir: SwapDir,
}

impl<'a> CycleLeg<'a> {
    pub fn new(quoter: &'a dyn Quoter, dir: SwapDir) -> Self {
        Self { quoter, dir }
    }
}

/// Chain `delta_in` of the base asset through every leg (each leg's NET output feeds the next),
/// returning the final NET output back in the base asset. `None` if any leg cannot quote.
pub fn cycle_net_out(legs: &[CycleLeg], delta_in: u64) -> Option<u64> {
    let mut amount = delta_in;
    for leg in legs {
        let out = leg
            .quoter
            .quote_exact_in(QuoteIn {
                dir: leg.dir,
                amount_in: amount,
            })
            .ok()?;
        amount = out.net_out;
        if amount == 0 {
            return Some(0);
        }
    }
    Some(amount)
}

/// Cycle profit (`final_out − delta_in`) as a signed integer; `None` on arithmetic failure.
pub fn cycle_profit(legs: &[CycleLeg], delta_in: u64) -> Option<i128> {
    let out = cycle_net_out(legs, delta_in)?;
    (out as i128).checked_sub(delta_in as i128)
}

/// Exact opportunity test for a cycle: is there ANY input size with positive profit? Cheap probe
/// (the marginal/spot product across the legs) — a `true` here means the ternary search should
/// run. Uses each leg's Q64.64 marginal price; `None` (undefined) legs are treated as no-edge.
pub fn cycle_opportunity_exists(legs: &[CycleLeg]) -> bool {
    // Product of per-leg marginal (pre-fee) prices; > 1.0 (in Q64.64, > 2^64 after N hops scaled)
    // means the round-trip returns more than it costs at the margin. Done in f64 — advisory only;
    // the integer ternary search is the gate-safe decider.
    let mut product = 1.0f64;
    for leg in legs {
        match leg.quoter.marginal_price_x64(leg.dir) {
            Some(px) => product *= px as f64 / (u64::MAX as f64 + 1.0),
            None => return false,
        }
    }
    product > 1.0
}

/// Size a cycle: ternary-search the profit-maximizing input over `[1, max_in]` (profit is unimodal
/// in size). Returns `(sized_delta_in, predicted_final_out, predicted_profit)`, or `None` if no
/// positive-profit size exists. `max_in` bounds the search (e.g. the base reserve of leg A).
pub fn size_cycle(legs: &[CycleLeg], max_in: u64) -> Option<(u64, u64, i128)> {
    if legs.len() < 2 || max_in < 2 {
        return None;
    }
    let profit = |d: u64| cycle_profit(legs, d).unwrap_or(i128::MIN);

    let mut lo: u64 = 1;
    let mut hi: u64 = max_in;

    // Ternary search: ~ceil(log_1.5(range)) steps; clamp to 256 as a hard backstop.
    for _ in 0..256 {
        if hi.saturating_sub(lo) <= 2 {
            break;
        }
        let third = (hi.saturating_sub(lo)) / 3;
        let m1 = lo.saturating_add(third);
        let m2 = hi.saturating_sub(third);
        if profit(m1) < profit(m2) {
            lo = m1.saturating_add(1);
        } else {
            hi = m2.saturating_sub(1);
        }
    }

    // Scan the small remaining window for the exact argmax.
    let mut best_d = lo;
    let mut best_p = profit(lo);
    let mut d = lo;
    while d <= hi {
        let p = profit(d);
        if p > best_p {
            best_p = p;
            best_d = d;
        }
        d = match d.checked_add(1) {
            Some(n) => n,
            None => break,
        };
    }

    if best_p > 0 {
        let out = cycle_net_out(legs, best_d)?;
        Some((best_d, out, best_p))
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::cpmm::CpmmReserves;
    use crate::venue::CpmmVenue;
    use arb_types::DexKind;

    fn cp(a: u64, b: u64) -> CpmmVenue {
        CpmmVenue::new(DexKind::RaydiumCpmm, CpmmReserves::new(a, b, 25, 10_000))
    }

    #[test]
    fn two_leg_cycle_matches_round_trip_shape() {
        // base→X via pool A (AtoB), X→base via pool B (AtoB): same dislocated pair as the 2-leg
        // RoundTrip test — a small size profits, an oversized one loses.
        let a = cp(1_000_000, 2_000_000);
        let b = cp(2_000_000, 1_100_000);
        let legs = [
            CycleLeg::new(&a, SwapDir::AtoB),
            CycleLeg::new(&b, SwapDir::AtoB),
        ];
        assert!(cycle_profit(&legs, 5_000).unwrap() > 0);
        assert!(cycle_profit(&legs, 50_000).unwrap() < 0);
        let (d, out, profit) = size_cycle(&legs, 1_000_000).expect("profit");
        assert!(profit > 0);
        assert_eq!(out as i128 - d as i128, profit);
        // The optimum beats its neighbours (true argmax).
        assert!(
            cycle_profit(&legs, d).unwrap() >= cycle_profit(&legs, d.saturating_sub(1)).unwrap()
        );
        assert!(cycle_profit(&legs, d).unwrap() >= cycle_profit(&legs, d + 1).unwrap());
    }

    #[test]
    fn three_leg_triangle_profits_on_a_dislocated_cycle() {
        // A triangle base(0)→A(1)→B(2)→base(0). Price around the loop multiplies to > 1, so a
        // small input returns more base than it cost.
        // Leg1 base→A: 1.0M base / 2.0M A. Leg2 A→B: 1.0M A / 2.0M B. Leg3 B→base: 1.0M B / 4.4M base.
        let l1 = cp(1_000_000, 2_000_000);
        let l2 = cp(1_000_000, 2_000_000);
        let l3 = cp(1_000_000, 4_400_000);
        let legs = [
            CycleLeg::new(&l1, SwapDir::AtoB),
            CycleLeg::new(&l2, SwapDir::AtoB),
            CycleLeg::new(&l3, SwapDir::AtoB),
        ];
        assert!(cycle_opportunity_exists(&legs));
        let (d, out, profit) = size_cycle(&legs, 1_000_000).expect("triangle profit");
        assert!(profit > 0, "out {out} in {d}");
        assert!(out > d);
        assert_eq!(out as i128 - d as i128, profit);
    }

    #[test]
    fn balanced_cycle_has_no_profitable_size() {
        // Equal pools around the loop ⇒ fees guarantee a loss at every size ⇒ None.
        let l1 = cp(1_000_000, 1_000_000);
        let l2 = cp(1_000_000, 1_000_000);
        let l3 = cp(1_000_000, 1_000_000);
        let legs = [
            CycleLeg::new(&l1, SwapDir::AtoB),
            CycleLeg::new(&l2, SwapDir::AtoB),
            CycleLeg::new(&l3, SwapDir::AtoB),
        ];
        assert!(!cycle_opportunity_exists(&legs));
        assert!(size_cycle(&legs, 1_000_000).is_none());
    }

    #[test]
    fn cycle_net_out_chains_net_amounts() {
        let l1 = cp(1_000_000, 2_000_000);
        let l2 = cp(2_000_000, 1_100_000);
        let legs = [
            CycleLeg::new(&l1, SwapDir::AtoB),
            CycleLeg::new(&l2, SwapDir::AtoB),
        ];
        // Manually chain the two legs and compare.
        let mid = l1
            .quote_exact_in(QuoteIn {
                dir: SwapDir::AtoB,
                amount_in: 5_000,
            })
            .unwrap()
            .net_out;
        let end = l2
            .quote_exact_in(QuoteIn {
                dir: SwapDir::AtoB,
                amount_in: mid,
            })
            .unwrap()
            .net_out;
        assert_eq!(cycle_net_out(&legs, 5_000), Some(end));
    }
}
