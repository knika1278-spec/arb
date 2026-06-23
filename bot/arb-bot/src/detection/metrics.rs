//! detection-8 — detection ingest metrics + ingest→edge latency.
//!
//! A lock-free counter/gauge book mirroring the executor [`crate::metrics::registry::MetricsRegistry`]
//! pattern: every writer is a pure `AtomicU64` op, safe to call from the ingest thread. Detection
//! latency (ingest→edge-recompute) is a first-class health metric (plan §5/§1), so it reuses the
//! audited exponential-bucket histogram from observ-2 ([`crate::metrics::latency::Histogram`]) rather
//! than duplicating the quantile math.
//!
//! Because every metric is a STRUCT FIELD (not a name registered into a global table), constructing
//! the book — even more than once — can never raise a duplicate-registration panic; it is simply
//! held once behind an `Arc` as part of the bot-wide shared metric set.

use core::sync::atomic::{AtomicU64, Ordering};

use arb_types::DexKind;

use crate::metrics::latency::Histogram;

/// Why the idempotent pool-state cache dropped a streamed update (`cache_rejected_total{reason}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheRejectReason {
    /// Strictly older slot than the cached state (firehose reordering / late delivery).
    StaleSlot,
    /// Same-or-not-newer `(slot, write_version)` within the session — a duplicate write.
    Duplicate,
}

/// Immutable read-out of the detection counters + gauges (off hot path).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetectionSnapshot {
    pub updates_total: u64,
    pub cache_rejected_total: u64,
    pub reconnects_total: u64,
    pub gap_reconciles_total: u64,
    pub decode_errors_total: u64,
    pub hot_pools: u64,
    pub stale_pools: u64,
}

/// Lock-free detection ingest metrics (detection-8). Construct once; share via `Arc`.
#[derive(Debug, Default)]
pub struct DetectionMetrics {
    updates_total: AtomicU64,
    // cache_rejected_total{reason}
    cache_rej_stale: AtomicU64,
    cache_rej_duplicate: AtomicU64,
    reconnects_total: AtomicU64,
    gap_reconciles_total: AtomicU64,
    // decode_errors_total{venue}
    decode_err_raydium_cpmm: AtomicU64,
    decode_err_orca_whirlpool: AtomicU64,
    decode_err_pumpswap: AtomicU64,
    // gauges (last-set value, not cumulative)
    hot_pools: AtomicU64,
    stale_pools: AtomicU64,
    /// ingest → edge-recompute latency histogram (P50/P95).
    ingest_to_edge: Histogram,
}

impl DetectionMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// An accepted streamed account update entered the pipeline.
    #[inline]
    pub fn record_update(&self) {
        self.updates_total.fetch_add(1, Ordering::Relaxed);
    }

    /// The idempotent cache dropped an update as stale/duplicate.
    #[inline]
    pub fn record_cache_reject(&self, reason: CacheRejectReason) {
        match reason {
            CacheRejectReason::StaleSlot => &self.cache_rej_stale,
            CacheRejectReason::Duplicate => &self.cache_rej_duplicate,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    /// The ingest supervisor re-established a dropped subscription (detection-7).
    #[inline]
    pub fn record_reconnect(&self) {
        self.reconnects_total.fetch_add(1, Ordering::Relaxed);
    }

    /// A post-reconnect gap was reconciled by resubscribing from the last processed slot.
    #[inline]
    pub fn record_gap_reconcile(&self) {
        self.gap_reconciles_total.fetch_add(1, Ordering::Relaxed);
    }

    /// A per-venue decoder rejected a buffer (bad discriminator / short / mis-shaped).
    #[inline]
    pub fn record_decode_error(&self, venue: DexKind) {
        match venue {
            DexKind::RaydiumCpmm => &self.decode_err_raydium_cpmm,
            DexKind::OrcaWhirlpool => &self.decode_err_orca_whirlpool,
            DexKind::PumpSwapAmm => &self.decode_err_pumpswap,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    /// Set the hot-pool gauge (pools updated within the freshness window).
    #[inline]
    pub fn set_hot_pools(&self, n: u64) {
        self.hot_pools.store(n, Ordering::Relaxed);
    }

    /// Set the stale-pool gauge (pools past the freshness window).
    #[inline]
    pub fn set_stale_pools(&self, n: u64) {
        self.stale_pools.store(n, Ordering::Relaxed);
    }

    /// Record an ingest→edge latency sample in microseconds.
    #[inline]
    pub fn record_ingest_to_edge_us(&self, us: f64) {
        self.ingest_to_edge.record_us(us);
    }

    /// ingest→edge P50 in milliseconds (`None` with no samples).
    pub fn ingest_to_edge_p50_ms(&self) -> Option<f64> {
        self.ingest_to_edge.p50_ms()
    }

    /// ingest→edge P95 in milliseconds (`None` with no samples).
    pub fn ingest_to_edge_p95_ms(&self) -> Option<f64> {
        self.ingest_to_edge.p95_ms()
    }

    /// ingest→edge sample count.
    pub fn ingest_to_edge_count(&self) -> u64 {
        self.ingest_to_edge.count()
    }

    /// Per-venue decode-error count.
    pub fn decode_error_count(&self, venue: DexKind) -> u64 {
        match venue {
            DexKind::RaydiumCpmm => &self.decode_err_raydium_cpmm,
            DexKind::OrcaWhirlpool => &self.decode_err_orca_whirlpool,
            DexKind::PumpSwapAmm => &self.decode_err_pumpswap,
        }
        .load(Ordering::Relaxed)
    }

    /// Per-reason cache-reject count.
    pub fn cache_reject_count(&self, reason: CacheRejectReason) -> u64 {
        match reason {
            CacheRejectReason::StaleSlot => &self.cache_rej_stale,
            CacheRejectReason::Duplicate => &self.cache_rej_duplicate,
        }
        .load(Ordering::Relaxed)
    }

    /// Off-hot-path read-out of the counters + gauges.
    pub fn snapshot(&self) -> DetectionSnapshot {
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        DetectionSnapshot {
            updates_total: load(&self.updates_total),
            cache_rejected_total: load(&self.cache_rej_stale) + load(&self.cache_rej_duplicate),
            reconnects_total: load(&self.reconnects_total),
            gap_reconciles_total: load(&self.gap_reconciles_total),
            decode_errors_total: load(&self.decode_err_raydium_cpmm)
                + load(&self.decode_err_orca_whirlpool)
                + load(&self.decode_err_pumpswap),
            hot_pools: load(&self.hot_pools),
            stale_pools: load(&self.stale_pools),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn ingest_to_edge_exposes_p50_p95() {
        let m = DetectionMetrics::new();
        assert_eq!(m.ingest_to_edge_p50_ms(), None); // no samples yet
        for i in 1..=1000 {
            m.record_ingest_to_edge_us(i as f64 * 1000.0); // 1ms..1000ms uniform
        }
        let p50 = m.ingest_to_edge_p50_ms().unwrap();
        let p95 = m.ingest_to_edge_p95_ms().unwrap();
        assert!((p50 - 500.5).abs() / 500.5 < 0.01, "p50={p50}");
        assert!((p95 - 950.05).abs() / 950.05 < 0.01, "p95={p95}");
        assert_eq!(m.ingest_to_edge_count(), 1000);
    }

    #[test]
    fn dedupe_rejects_and_reconnects_are_counted() {
        let m = DetectionMetrics::new();
        m.record_cache_reject(CacheRejectReason::StaleSlot);
        m.record_cache_reject(CacheRejectReason::Duplicate);
        m.record_cache_reject(CacheRejectReason::Duplicate);
        m.record_reconnect();
        m.record_gap_reconcile();
        assert_eq!(m.cache_reject_count(CacheRejectReason::StaleSlot), 1);
        assert_eq!(m.cache_reject_count(CacheRejectReason::Duplicate), 2);
        let s = m.snapshot();
        assert_eq!(s.cache_rejected_total, 3);
        assert_eq!(s.reconnects_total, 1);
        assert_eq!(s.gap_reconciles_total, 1);
    }

    #[test]
    fn per_venue_decode_errors_increment_independently() {
        let m = DetectionMetrics::new();
        m.record_decode_error(DexKind::OrcaWhirlpool);
        m.record_decode_error(DexKind::OrcaWhirlpool);
        m.record_decode_error(DexKind::PumpSwapAmm);
        assert_eq!(m.decode_error_count(DexKind::OrcaWhirlpool), 2);
        assert_eq!(m.decode_error_count(DexKind::PumpSwapAmm), 1);
        assert_eq!(m.decode_error_count(DexKind::RaydiumCpmm), 0);
        assert_eq!(m.snapshot().decode_errors_total, 3);
    }

    #[test]
    fn gauges_hold_last_set_value() {
        let m = DetectionMetrics::new();
        m.set_hot_pools(42);
        m.set_stale_pools(7);
        m.set_hot_pools(40); // gauge: last value wins, not cumulative
        let s = m.snapshot();
        assert_eq!(s.hot_pools, 40);
        assert_eq!(s.stale_pools, 7);
    }

    #[test]
    fn constructs_without_duplicate_registration_panic() {
        // Field-struct metrics can never raise a duplicate-registration panic, unlike a global
        // name-keyed registry — two independent books coexist cleanly.
        let a = DetectionMetrics::new();
        let b = DetectionMetrics::new();
        a.record_update();
        assert_eq!(a.snapshot().updates_total, 1);
        assert_eq!(b.snapshot().updates_total, 0);
    }

    #[test]
    fn concurrent_writers_produce_correct_counts() {
        let m = Arc::new(DetectionMetrics::new());
        let threads = 8;
        let per = 10_000;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    for _ in 0..per {
                        m.record_update();
                        m.record_decode_error(DexKind::RaydiumCpmm);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(m.snapshot().updates_total, threads * per);
        assert_eq!(m.decode_error_count(DexKind::RaydiumCpmm), threads * per);
    }
}
