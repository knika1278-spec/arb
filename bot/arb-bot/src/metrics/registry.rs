//! observ-1 — lock-free, allocation-free in-process metric registry.
//!
//! The hot-path writers (`record_attempt`/`record_land`/`record_revert`) are pure `AtomicU64`
//! fetch-adds: no locks, no heap, safe to call from any thread on the sign/land path. The
//! per-route latency and slippage books are owned here so one `MetricsRegistry` is the single
//! handle the bot threads share (behind an `Arc`). Windowed rates (revert-rate over 5 min,
//! burn-rate/min) are NOT here — they come from the timestamped [`super::pnl::PnlLedger`]; this
//! registry holds the cumulative counters and the streaming latency/slippage sketches.

use core::sync::atomic::{AtomicU64, Ordering};

use super::latency::{LatencyBook, LatencyStage, SpanGuard};
use super::slippage::SlippageBook;
use super::types::{RevertCause, RouteKey};

/// Cumulative, lock-free counters + the latency/slippage books. Construct once, share via `Arc`.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    attempts: AtomicU64,
    lands: AtomicU64,
    reverts: AtomicU64,
    /// Cumulative gross profit (lamports) over landed txs.
    landed_profit_lamports: AtomicU64,
    /// Cumulative base+priority burned over reverted losers that actually reached a block.
    burned_lamports: AtomicU64,
    // Drop-cause histogram (one bucket per RevertCause).
    revert_tip_lost: AtomicU64,
    revert_congestion: AtomicU64,
    revert_stale_blockhash: AtomicU64,
    revert_sim_fail: AtomicU64,
    revert_onchain_unprofitable: AtomicU64,
    revert_unknown: AtomicU64,
    // Confirmation rank of the most recent land (plan §9.4: capture (slot, index_in_block)).
    last_slot: AtomicU64,
    last_index_in_block: AtomicU64,
    /// Per-stage submit-latency histograms (P50/P95).
    pub latency: LatencyBook,
    /// Per-route realized-vs-predicted slippage book.
    pub slippage: SlippageBook,
}

/// An immutable read-out of the cumulative counters (off hot path).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub attempts: u64,
    pub lands: u64,
    pub reverts: u64,
    pub landed_profit_lamports: u64,
    pub burned_lamports: u64,
    pub last_slot: u64,
    pub last_index_in_block: u64,
}

impl CounterSnapshot {
    /// Cumulative revert-rate as a fraction of *resolved* attempts (`reverts / (lands+reverts)`).
    /// Returns 0 with no resolved attempts. Windowed revert-rate (the kill-switch input) lives in
    /// [`super::pnl::PnlLedger`]; this is the lifetime figure for the dashboard.
    pub fn revert_rate_pct(&self) -> f64 {
        let resolved = self.lands + self.reverts;
        if resolved == 0 {
            0.0
        } else {
            (self.reverts as f64) * 100.0 / (resolved as f64)
        }
    }
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hot-path: an arb attempt is about to be signed/submitted. Lock-free.
    #[inline]
    pub fn record_attempt(&self) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Hot-path: a bundle landed profitably. Records gross profit + confirmation rank. Lock-free.
    #[inline]
    pub fn record_land(&self, profit_lamports: u64, slot: u64, index_in_block: u32) {
        self.lands.fetch_add(1, Ordering::Relaxed);
        self.landed_profit_lamports
            .fetch_add(profit_lamports, Ordering::Relaxed);
        self.last_slot.store(slot, Ordering::Relaxed);
        self.last_index_in_block
            .store(index_in_block as u64, Ordering::Relaxed);
    }

    /// Hot-path: an attempt reverted/dropped. `burned_lamports` is base+priority IFF the tx
    /// actually reached a block (caller passes 0 for pre-inclusion drops). Lock-free.
    #[inline]
    pub fn record_revert(&self, cause: RevertCause, burned_lamports: u64) {
        self.reverts.fetch_add(1, Ordering::Relaxed);
        self.burned_lamports
            .fetch_add(burned_lamports, Ordering::Relaxed);
        let bucket = match cause {
            RevertCause::TipLost => &self.revert_tip_lost,
            RevertCause::Congestion => &self.revert_congestion,
            RevertCause::StaleBlockhash => &self.revert_stale_blockhash,
            RevertCause::SimFail => &self.revert_sim_fail,
            RevertCause::OnchainUnprofitable => &self.revert_onchain_unprofitable,
            RevertCause::Unknown => &self.revert_unknown,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    /// Start a RAII latency span for `stage`; the elapsed time is recorded on drop.
    #[inline]
    pub fn start_span(&self, stage: LatencyStage) -> SpanGuard<'_> {
        self.latency.start_span(stage)
    }

    /// Post-land: record signed-bps deviation of realized vs bit-exact predicted output.
    #[inline]
    pub fn record_realized_slippage(&self, route: RouteKey, predicted_out: u64, realized_out: u64) {
        self.slippage.record(route, predicted_out, realized_out);
    }

    /// Read the cumulative counters (off hot path).
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            attempts: self.attempts.load(Ordering::Relaxed),
            lands: self.lands.load(Ordering::Relaxed),
            reverts: self.reverts.load(Ordering::Relaxed),
            landed_profit_lamports: self.landed_profit_lamports.load(Ordering::Relaxed),
            burned_lamports: self.burned_lamports.load(Ordering::Relaxed),
            last_slot: self.last_slot.load(Ordering::Relaxed),
            last_index_in_block: self.last_index_in_block.load(Ordering::Relaxed),
        }
    }

    /// Drop-cause count for one cause (dashboard/test).
    pub fn revert_cause_count(&self, cause: RevertCause) -> u64 {
        let bucket = match cause {
            RevertCause::TipLost => &self.revert_tip_lost,
            RevertCause::Congestion => &self.revert_congestion,
            RevertCause::StaleBlockhash => &self.revert_stale_blockhash,
            RevertCause::SimFail => &self.revert_sim_fail,
            RevertCause::OnchainUnprofitable => &self.revert_onchain_unprofitable,
            RevertCause::Unknown => &self.revert_unknown,
        };
        bucket.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn counters_accumulate() {
        let r = MetricsRegistry::new();
        r.record_attempt();
        r.record_attempt();
        r.record_land(1_000, 42, 3);
        r.record_revert(RevertCause::OnchainUnprofitable, 7_000);
        let s = r.snapshot();
        assert_eq!(s.attempts, 2);
        assert_eq!(s.lands, 1);
        assert_eq!(s.reverts, 1);
        assert_eq!(s.landed_profit_lamports, 1_000);
        assert_eq!(s.burned_lamports, 7_000);
        assert_eq!(s.last_slot, 42);
        assert_eq!(s.last_index_in_block, 3);
        // One land, one revert => 50% lifetime revert-rate.
        assert!((s.revert_rate_pct() - 50.0).abs() < 1e-9);
        assert_eq!(r.revert_cause_count(RevertCause::OnchainUnprofitable), 1);
        assert_eq!(r.revert_cause_count(RevertCause::TipLost), 0);
    }

    #[test]
    fn pre_inclusion_drop_burns_nothing() {
        let r = MetricsRegistry::new();
        // Tip-auction loss never reaches a block => 0 burned (plan §2 "biaya nol").
        r.record_revert(RevertCause::TipLost, 0);
        assert_eq!(r.snapshot().burned_lamports, 0);
        assert!(!RevertCause::TipLost.burned_fees());
        assert!(RevertCause::OnchainUnprofitable.burned_fees());
    }

    #[test]
    fn concurrent_writers_produce_correct_counts() {
        let r = Arc::new(MetricsRegistry::new());
        let threads = 8;
        let per_thread = 10_000;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let r = Arc::clone(&r);
                thread::spawn(move || {
                    for _ in 0..per_thread {
                        r.record_attempt();
                        r.record_revert(RevertCause::Congestion, 5_000);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let s = r.snapshot();
        assert_eq!(s.attempts, threads * per_thread);
        assert_eq!(s.reverts, threads * per_thread);
        assert_eq!(s.burned_lamports, threads * per_thread * 5_000);
        assert_eq!(
            r.revert_cause_count(RevertCause::Congestion),
            threads * per_thread
        );
    }
}
