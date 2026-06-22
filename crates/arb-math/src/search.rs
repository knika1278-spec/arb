//! Exact integer optimal-size search. The round-trip profit function is strictly concave
//! in input size (plan.md §7), so a ternary search over the **exact integer** profit finds
//! the true optimum — no float, no overflow, deterministic. This is the gate-safe primary
//! (the f64 closed form in [`crate::optimal`] is only a fast hint), and it is the same
//! routine the CLMM/DLMM venues will reuse in Fase 3 (profit unimodal).

use crate::cpmm::RoundTrip;
use arb_types::SwapDir;

/// Upper bound for the search: you would never input more base than pool A's input-side
/// reserve, and profit is long past its peak well before that.
fn search_ceiling(rt: &RoundTrip) -> u64 {
    match rt.dir_a {
        SwapDir::AtoB => rt.pool_a.reserve_a,
        SwapDir::BtoA => rt.pool_a.reserve_b,
    }
}

/// Returns `(optimal_delta_in, profit_at_optimum)` maximizing `rt.profit`, or `None` if no
/// positive-profit size exists. Caps iterations for a bounded CU/latency budget.
pub fn optimal_delta_search(rt: &RoundTrip) -> Option<(u64, i128)> {
    let hi0 = search_ceiling(rt);
    if hi0 < 2 {
        return None;
    }
    let profit = |d: u64| rt.profit(d).unwrap_or(i128::MIN);

    let mut lo: u64 = 1;
    let mut hi: u64 = hi0.saturating_sub(1);

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
        Some((best_d, best_p))
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::cpmm::CpmmReserves;

    #[test]
    fn finds_peak_and_beats_neighbors() {
        let a = CpmmReserves::new(1_000_000, 2_000_000, 25, 10_000);
        let b = CpmmReserves::new(2_000_000, 1_100_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB);
        let (d, p) = optimal_delta_search(&rt).expect("opportunity");
        assert!(p > 0);
        // Optimum: neighbors must not be strictly better.
        assert!(rt.profit(d).unwrap() >= rt.profit(d.saturating_sub(1)).unwrap());
        assert!(rt.profit(d).unwrap() >= rt.profit(d + 1).unwrap());
    }

    #[test]
    fn none_when_no_arbitrage() {
        let a = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let b = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::BtoA);
        assert_eq!(optimal_delta_search(&rt), None);
    }
}
