//! observ-2 — submit-latency P50/P95 per pipeline stage + confirmation-rank capture.
//!
//! Recording is lock-free: each stage is an exponential-bucket histogram of `AtomicU64`s, so a
//! [`SpanGuard`] drop is a single `fetch_add`. Quantiles are computed off the hot path by walking
//! the buckets with linear in-bucket interpolation (≈geometric buckets at 2% width ⇒ <1% quantile
//! error on a smooth distribution). A [`SpanGuard`] observes its stage on `Drop`, so it records
//! even on early return or panic-unwind.

use core::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Histogram floor: samples below this clamp to bucket 0.
const MIN_US: f64 = 50.0;
/// Geometric bucket growth factor (2% width).
const FACTOR: f64 = 1.02;
/// Bucket count: `50µs · 1.02^700 ≈ 52s` ceiling covers any sane submit/land latency.
const N_BUCKETS: usize = 700;

/// Pipeline stage a latency span measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyStage {
    /// Geyser update → built tx plan.
    DetectToBuild,
    /// Built plan → simulation done.
    BuildToSim,
    /// Simulation → submitted to a landing route.
    SimToSubmit,
    /// Submitted → landed/confirmed.
    SubmitToLand,
}

impl LatencyStage {
    /// All stages in canonical order (for exporters that iterate every histogram).
    pub const ALL: [LatencyStage; 4] = [
        LatencyStage::DetectToBuild,
        LatencyStage::BuildToSim,
        LatencyStage::SimToSubmit,
        LatencyStage::SubmitToLand,
    ];

    const fn index(self) -> usize {
        match self {
            LatencyStage::DetectToBuild => 0,
            LatencyStage::BuildToSim => 1,
            LatencyStage::SimToSubmit => 2,
            LatencyStage::SubmitToLand => 3,
        }
    }

    /// Stable label for metric keys.
    pub const fn label(self) -> &'static str {
        match self {
            LatencyStage::DetectToBuild => "detect_to_build",
            LatencyStage::BuildToSim => "build_to_sim",
            LatencyStage::SimToSubmit => "sim_to_submit",
            LatencyStage::SubmitToLand => "submit_to_land",
        }
    }
}

/// Confirmation rank of a landed bundle (plan §9.4: pin `(slot, index_in_block)`, asserted
/// populated, not defaulted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmationRank {
    pub slot: u64,
    pub index_in_block: u32,
}

#[derive(Debug)]
struct Histogram {
    buckets: Vec<AtomicU64>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: (0..N_BUCKETS).map(|_| AtomicU64::new(0)).collect(),
        }
    }
}

impl Histogram {
    #[inline]
    fn record_us(&self, us: f64) {
        let idx = if us <= MIN_US {
            0
        } else {
            let i = ((us / MIN_US).ln() / FACTOR.ln()) as usize;
            i.min(N_BUCKETS - 1)
        };
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Linear-interpolated quantile in microseconds. Returns `None` with no samples.
    fn quantile_us(&self, q: f64) -> Option<f64> {
        let counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return None;
        }
        let target = q.clamp(0.0, 1.0) * total as f64;
        let mut cum = 0.0f64;
        for (b, &c) in counts.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let next = cum + c as f64;
            if next >= target {
                let frac = ((target - cum) / c as f64).clamp(0.0, 1.0);
                let lo = MIN_US * FACTOR.powi(b as i32);
                let hi = lo * FACTOR;
                return Some(lo + (hi - lo) * frac);
            }
            cum = next;
        }
        // Numerical edge: return the top occupied bucket's high edge.
        Some(MIN_US * FACTOR.powi(N_BUCKETS as i32))
    }

    fn count(&self) -> u64 {
        self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).sum()
    }
}

/// One histogram per [`LatencyStage`] plus the last confirmation rank.
#[derive(Debug)]
pub struct LatencyBook {
    stages: [Histogram; 4],
    last_rank_slot: AtomicU64,
    last_rank_index: AtomicU64,
    has_rank: AtomicU64,
}

impl Default for LatencyBook {
    fn default() -> Self {
        Self {
            stages: [
                Histogram::default(),
                Histogram::default(),
                Histogram::default(),
                Histogram::default(),
            ],
            last_rank_slot: AtomicU64::new(0),
            last_rank_index: AtomicU64::new(0),
            has_rank: AtomicU64::new(0),
        }
    }
}

impl LatencyBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a raw microsecond sample for `stage` (lock-free). Used directly by tests and by
    /// [`SpanGuard`] on drop.
    #[inline]
    pub fn record_us(&self, stage: LatencyStage, us: f64) {
        self.stages[stage.index()].record_us(us);
    }

    /// Start a RAII span that records elapsed time into `stage` on drop.
    #[inline]
    pub fn start_span(&self, stage: LatencyStage) -> SpanGuard<'_> {
        SpanGuard {
            book: self,
            stage,
            start: Instant::now(),
        }
    }

    /// P50 for a stage in milliseconds (`None` if no samples).
    pub fn p50_ms(&self, stage: LatencyStage) -> Option<f64> {
        self.stages[stage.index()]
            .quantile_us(0.50)
            .map(|us| us / 1000.0)
    }

    /// P95 for a stage in milliseconds (`None` if no samples).
    pub fn p95_ms(&self, stage: LatencyStage) -> Option<f64> {
        self.stages[stage.index()]
            .quantile_us(0.95)
            .map(|us| us / 1000.0)
    }

    /// Sample count for a stage.
    pub fn count(&self, stage: LatencyStage) -> u64 {
        self.stages[stage.index()].count()
    }

    /// Record the confirmation rank of a landed bundle.
    pub fn record_confirmation(&self, rank: ConfirmationRank) {
        self.last_rank_slot.store(rank.slot, Ordering::Relaxed);
        self.last_rank_index
            .store(rank.index_in_block as u64, Ordering::Relaxed);
        self.has_rank.store(1, Ordering::Relaxed);
    }

    /// The most recent confirmation rank, or `None` if none captured yet (asserted populated, not
    /// silently defaulted to 0/0).
    pub fn last_confirmation(&self) -> Option<ConfirmationRank> {
        if self.has_rank.load(Ordering::Relaxed) == 0 {
            None
        } else {
            Some(ConfirmationRank {
                slot: self.last_rank_slot.load(Ordering::Relaxed),
                index_in_block: self.last_rank_index.load(Ordering::Relaxed) as u32,
            })
        }
    }
}

/// RAII latency timer. Records elapsed time into its stage on drop — even on early return or
/// panic-unwind (the workspace builds with `panic = "unwind"`).
#[derive(Debug)]
pub struct SpanGuard<'a> {
    book: &'a LatencyBook,
    stage: LatencyStage,
    start: Instant,
}

impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        let us = self.start.elapsed().as_secs_f64() * 1_000_000.0;
        self.book.record_us(self.stage, us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p50_p95_within_one_percent_on_uniform_distribution() {
        let book = LatencyBook::new();
        // Uniform over [1ms, 1000ms] => true P50≈500.5ms, P95≈950.05ms.
        for i in 1..=1000 {
            book.record_us(LatencyStage::SubmitToLand, i as f64 * 1000.0);
        }
        let p50 = book.p50_ms(LatencyStage::SubmitToLand).unwrap();
        let p95 = book.p95_ms(LatencyStage::SubmitToLand).unwrap();
        assert!((p50 - 500.5).abs() / 500.5 < 0.01, "p50={p50}");
        assert!((p95 - 950.05).abs() / 950.05 < 0.01, "p95={p95}");
        assert_eq!(book.count(LatencyStage::SubmitToLand), 1000);
    }

    #[test]
    fn span_guard_records_on_drop_including_early_return() {
        let book = LatencyBook::new();
        fn early(book: &LatencyBook) {
            let _g = book.start_span(LatencyStage::DetectToBuild);
            // early return: the guard still drops and records.
        }
        early(&book);
        assert_eq!(book.count(LatencyStage::DetectToBuild), 1);
        // Other stages untouched.
        assert_eq!(book.count(LatencyStage::BuildToSim), 0);
    }

    #[test]
    fn confirmation_rank_is_none_until_captured() {
        let book = LatencyBook::new();
        assert_eq!(book.last_confirmation(), None);
        book.record_confirmation(ConfirmationRank {
            slot: 1234,
            index_in_block: 7,
        });
        assert_eq!(
            book.last_confirmation(),
            Some(ConfirmationRank {
                slot: 1234,
                index_in_block: 7
            })
        );
    }

    #[test]
    fn empty_histogram_returns_none() {
        let book = LatencyBook::new();
        assert_eq!(book.p50_ms(LatencyStage::SimToSubmit), None);
    }
}
