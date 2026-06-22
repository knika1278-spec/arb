//! observ-9 — realized slippage per route.
//!
//! On every landed tx we compare the bit-exact predicted output (from the sizing mirror) against
//! the realized output (balance delta) and store the signed bps deviation, bucketed by
//! `(venue_pair, direction)`. Because `predicted_out` comes from the M1-GATE bit-exact mirror, a
//! nonzero realized slippage is a *real* signal (decode drift / stale reserves), not rounding
//! noise — once the mirror is green, `predicted == realized` ⇒ recorded 0 bps.
//!
//! Recording happens post-land (off the sign hot path), so a short mutex around the per-route map
//! is acceptable; the sign/land counters that must stay lock-free live in [`super::registry`].

use std::collections::HashMap;
use std::sync::Mutex;

use super::types::RouteKey;

/// Aggregate slippage stats for one route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlipStats {
    pub samples: u64,
    /// Sum of signed bps (positive = realized below predicted, i.e. we got *less* than predicted).
    pub sum_bps: i128,
    pub min_bps: i64,
    pub max_bps: i64,
    pub last_bps: i64,
}

impl SlipStats {
    pub fn mean_bps(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.sum_bps as f64 / self.samples as f64
        }
    }
}

#[derive(Debug, Default)]
pub struct SlippageBook {
    by_route: Mutex<HashMap<RouteKey, SlipStats>>,
}

/// Signed bps deviation of realized vs predicted: `(predicted - realized) * 10000 / predicted`,
/// saturating into `i64`. Positive ⇒ realized came in *below* predicted. `predicted == 0 ⇒ 0`.
pub fn slippage_bps(predicted_out: u64, realized_out: u64) -> i64 {
    if predicted_out == 0 {
        return 0;
    }
    let num = (predicted_out as i128 - realized_out as i128) * 10_000;
    let bps = num / predicted_out as i128;
    bps.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

impl SlippageBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one landed tx's predicted-vs-realized output for `route`.
    pub fn record(&self, route: RouteKey, predicted_out: u64, realized_out: u64) {
        let bps = slippage_bps(predicted_out, realized_out);
        let mut map = self.by_route.lock().unwrap();
        let s = map.entry(route).or_insert(SlipStats {
            min_bps: i64::MAX,
            max_bps: i64::MIN,
            ..SlipStats::default()
        });
        s.samples += 1;
        s.sum_bps += bps as i128;
        s.min_bps = s.min_bps.min(bps);
        s.max_bps = s.max_bps.max(bps);
        s.last_bps = bps;
    }

    /// Read the aggregate stats for one route (`None` if never recorded).
    pub fn stats(&self, route: &RouteKey) -> Option<SlipStats> {
        self.by_route.lock().unwrap().get(route).copied()
    }

    /// Number of distinct routes observed.
    pub fn route_count(&self) -> usize {
        self.by_route.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::SwapDir;
    use solana_pubkey::Pubkey;

    fn route(a: u8, b: u8, dir: SwapDir) -> RouteKey {
        RouteKey::new(
            Pubkey::new_from_array([a; 32]),
            Pubkey::new_from_array([b; 32]),
            dir,
        )
    }

    #[test]
    fn bit_exact_mirror_records_zero_slippage() {
        let book = SlippageBook::new();
        let r = route(1, 2, SwapDir::AtoB);
        book.record(r, 1_000_000, 1_000_000);
        let s = book.stats(&r).unwrap();
        assert_eq!(s.samples, 1);
        assert_eq!(s.last_bps, 0);
        assert_eq!(s.mean_bps(), 0.0);
    }

    #[test]
    fn signed_bps_formula() {
        // realized 1% below predicted => +100 bps.
        assert_eq!(slippage_bps(1_000_000, 990_000), 100);
        // realized above predicted (got more) => negative bps.
        assert_eq!(slippage_bps(1_000_000, 1_010_000), -100);
        assert_eq!(slippage_bps(0, 5), 0);
    }

    #[test]
    fn distinct_routes_bucket_independently() {
        let book = SlippageBook::new();
        let r1 = route(1, 2, SwapDir::AtoB);
        let r2 = route(1, 2, SwapDir::BtoA); // same pair, opposite direction => distinct bucket
        book.record(r1, 1_000_000, 990_000); // +100 bps
        book.record(r2, 1_000_000, 980_000); // +200 bps
        assert_eq!(book.route_count(), 2);
        assert_eq!(book.stats(&r1).unwrap().last_bps, 100);
        assert_eq!(book.stats(&r2).unwrap().last_bps, 200);
    }
}
