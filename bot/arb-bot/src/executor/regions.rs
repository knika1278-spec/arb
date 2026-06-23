//! landing-2 — per-region rate limiting + latency-ranked fan-out for the Jito Block Engine.
//!
//! Jito documents a **1 req/s per-region** limit for the public auth tier; [`RegionRateLimiter`]
//! enforces it locally (logical-clock based, like the signer pre-sign caps — deterministic, no
//! syscalls) so we never burn the budget and eat a 429. [`RegionRanker`] orders the 8 regions
//! nearest-first from measured round-trip latency, and [`RegionRanker::fan_out_set`] picks the
//! nearest region plus N runners-up to submit the same bundle to (a landed-once race; the bundle
//! is idempotent on the leader).

use std::collections::HashMap;

use super::types::Region;

/// Local per-region rate limiter. `try_acquire` returns `true` and records the send iff at least
/// `min_interval_millis` has elapsed since this region's last send.
#[derive(Clone, Debug)]
pub struct RegionRateLimiter {
    min_interval_millis: u64,
    last_sent_millis: HashMap<Region, u64>,
}

impl RegionRateLimiter {
    /// `per_region_rps` requests/second/region (Jito's documented public limit is 1).
    pub fn new(per_region_rps: u32) -> Self {
        let rps = per_region_rps.max(1) as u64;
        Self {
            min_interval_millis: 1_000 / rps,
            last_sent_millis: HashMap::new(),
        }
    }

    /// Try to consume a send slot for `region` at `now_millis`. Records the send on success.
    pub fn try_acquire(&mut self, region: Region, now_millis: u64) -> bool {
        let ok = match self.last_sent_millis.get(&region) {
            Some(&last) => now_millis.saturating_sub(last) >= self.min_interval_millis,
            None => true,
        };
        if ok {
            self.last_sent_millis.insert(region, now_millis);
        }
        ok
    }
}

/// Ranks the 8 regions nearest-first from measured latency. Unprobed regions sort last (so a fresh
/// client still fans out, just without a latency preference).
#[derive(Clone, Debug, Default)]
pub struct RegionRanker {
    latency_millis: HashMap<Region, u64>,
}

impl RegionRanker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a measured round-trip latency probe for `region` (lower = nearer).
    pub fn record_latency(&mut self, region: Region, millis: u64) {
        self.latency_millis.insert(region, millis);
    }

    /// All 8 regions, nearest (lowest measured latency) first; unprobed regions keep their
    /// canonical order at the tail.
    pub fn ranked(&self) -> Vec<Region> {
        let mut probed: Vec<Region> = Region::ALL
            .into_iter()
            .filter(|r| self.latency_millis.contains_key(r))
            .collect();
        probed.sort_by_key(|r| self.latency_millis[r]);
        let unprobed = Region::ALL
            .into_iter()
            .filter(|r| !self.latency_millis.contains_key(r));
        probed.into_iter().chain(unprobed).collect()
    }

    /// The fan-out target set: the nearest region plus the next `extra` runners-up.
    pub fn fan_out_set(&self, extra: usize) -> Vec<Region> {
        let ranked = self.ranked();
        let take = (extra + 1).min(ranked.len());
        ranked[..take].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_caps_one_per_second_per_region() {
        let mut rl = RegionRateLimiter::new(1);
        assert!(rl.try_acquire(Region::Frankfurt, 0)); // first allowed
        assert!(!rl.try_acquire(Region::Frankfurt, 500)); // <1s later: denied
        assert!(rl.try_acquire(Region::Frankfurt, 1_000)); // exactly 1s: allowed
                                                           // A different region has its own bucket.
        assert!(rl.try_acquire(Region::Amsterdam, 500));
    }

    #[test]
    fn ranker_orders_nearest_first_unprobed_last() {
        let mut r = RegionRanker::new();
        r.record_latency(Region::Tokyo, 120);
        r.record_latency(Region::Frankfurt, 30);
        r.record_latency(Region::Ny, 60);
        let ranked = r.ranked();
        // Nearest-first among probed.
        assert_eq!(
            &ranked[..3],
            &[Region::Frankfurt, Region::Ny, Region::Tokyo]
        );
        // All 8 present; the 5 unprobed are at the tail.
        assert_eq!(ranked.len(), 8);
        assert!(!ranked[3..].contains(&Region::Frankfurt));
    }

    #[test]
    fn fan_out_takes_nearest_plus_extra() {
        let mut r = RegionRanker::new();
        r.record_latency(Region::Frankfurt, 30);
        r.record_latency(Region::Ny, 60);
        r.record_latency(Region::Amsterdam, 45);
        // nearest + 1 runner-up = the two lowest-latency regions, nearest first.
        assert_eq!(r.fan_out_set(1), vec![Region::Frankfurt, Region::Amsterdam]);
        // extra=0 => just the nearest.
        assert_eq!(r.fan_out_set(0), vec![Region::Frankfurt]);
    }
}
