//! Closed-form optimal round-trip size (plan.md §7). This is a **heuristic hint** only:
//! it picks a starting size; the exact integer optimum comes from [`crate::search`], the
//! 90–95% policy from [`crate::policy`], and the on-chain assert is the final safety net.
//! The closed form needs a real square root, so it lives behind the `std` feature.
//!
//! Variables (round-trip X -> pool A -> Y -> pool B -> X):
//!   Ra_in, Ra_out = oriented reserves of pool A; Rb_in, Rb_out = oriented reserves of B.
//!   g = (denom - num)/denom per pool.
//!   delta* = ( sqrt(ga·gb·Ra_in·Ra_out·Rb_in·Rb_out) − Ra_in·Rb_in )
//!            / ( ga·Rb_in + ga·gb·Ra_out )

use crate::cpmm::RoundTrip;
use arb_types::SwapDir;

#[cfg(feature = "std")]
fn oriented(p: &crate::cpmm::CpmmReserves, dir: SwapDir) -> (f64, f64, f64) {
    let (ri, ro) = match dir {
        SwapDir::AtoB => (p.reserve_a, p.reserve_b),
        SwapDir::BtoA => (p.reserve_b, p.reserve_a),
    };
    let g = (p.fee_denominator.saturating_sub(p.fee_numerator) as f64) / (p.fee_denominator as f64);
    (ri as f64, ro as f64, g)
}

/// Closed-form optimum (f64). Returns `None` when no profitable size exists (the
/// numerator is non-positive) or the inputs are degenerate.
#[cfg(feature = "std")]
pub fn optimal_delta_general(rt: &RoundTrip) -> Option<u64> {
    let (ra_in, ra_out, ga) = oriented(&rt.pool_a, rt.dir_a);
    let (rb_in, rb_out, gb) = oriented(&rt.pool_b, rt.dir_b);
    if ra_in <= 0.0 || ra_out <= 0.0 || rb_in <= 0.0 || rb_out <= 0.0 {
        return None;
    }
    let radicand = ga * gb * ra_in * ra_out * rb_in * rb_out;
    if radicand <= 0.0 {
        return None;
    }
    let numerator = radicand.sqrt() - ra_in * rb_in;
    if numerator <= 0.0 {
        return None; // no arbitrage in this direction
    }
    let denominator = ga * rb_in + ga * gb * ra_out;
    if denominator <= 0.0 {
        return None;
    }
    let delta = numerator / denominator;
    if !delta.is_finite() || delta < 1.0 {
        return None;
    }
    Some(delta.floor() as u64)
}

#[cfg(test)]
#[cfg(feature = "std")]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::cpmm::CpmmReserves;
    use crate::search::optimal_delta_search;

    #[test]
    fn closed_form_is_near_the_integer_optimum() {
        let a = CpmmReserves::new(1_000_000, 2_000_000, 25, 10_000);
        let b = CpmmReserves::new(2_000_000, 1_100_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB);

        let closed = optimal_delta_general(&rt).expect("opportunity exists");
        let (exact, exact_profit) = optimal_delta_search(&rt).expect("opportunity exists");

        // The closed form should land within a few percent of the exact integer optimum.
        let lo = exact.saturating_sub(exact / 20).max(1);
        let hi = exact + exact / 20 + 2;
        assert!(
            closed >= lo && closed <= hi,
            "closed={closed} exact={exact}"
        );
        // And the profit at the closed-form size should be within ~1% of the peak.
        let closed_profit = rt.profit(closed).unwrap();
        assert!(closed_profit * 100 >= exact_profit * 99);
    }

    #[test]
    fn no_opportunity_returns_none() {
        let a = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let b = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::BtoA);
        assert_eq!(optimal_delta_general(&rt), None);
    }
}
