//! Detection module: Geyser/gRPC ingest → idempotent pool-state cache → token-pair graph →
//! `DetectionSignal`. This wires the pure pieces (`cache`, `graph`, `decode`, `reconnect`,
//! `model`) into a `DetectionPipeline`; the async run-loop over a real `AccountUpdateSource`
//! attaches on top (detection-7).

pub mod cache;
pub mod decode;
pub mod graph;
pub mod grpc;
pub mod metrics;
pub mod model;
pub mod reconnect;

pub use cache::{accept_predicate, ApplyOutcome, PoolStateCache};
pub use graph::PairGraph;
pub use grpc::{AccountUpdateSource, RawAccountUpdate};
pub use metrics::{CacheRejectReason, DetectionMetrics, DetectionSnapshot};
pub use model::{canonical_pair, DetectionSignal, EdgeUpdate, PoolQuote, PriceView, SessionStamp};

use solana_pubkey::Pubkey;

/// Owns the cache + graph and turns accepted pool updates into detection signals.
#[derive(Default)]
pub struct DetectionPipeline {
    pub cache: PoolStateCache,
    pub graph: PairGraph,
    last_processed_slot: Option<u64>,
}

impl DetectionPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a decoded pool view stamped with its `(session, slot, write_version)`. Returns a
    /// `DetectionSignal` when the update is accepted (not stale) AND its pair now dislocates.
    pub fn on_pool_update(
        &mut self,
        pool: Pubkey,
        stamp: SessionStamp,
        view: PriceView,
    ) -> Option<DetectionSignal> {
        if !self.cache.apply(pool, stamp, view) {
            return None; // stale / duplicate
        }
        self.last_processed_slot = Some(
            self.last_processed_slot
                .map_or(stamp.slot, |s| s.max(stamp.slot)),
        );
        self.graph
            .on_event(pool, view)
            .map(DetectionSignal::EdgeUpdated)
    }

    /// Instrumented [`Self::on_pool_update`] (detection-8): records the accepted-update count and
    /// the ingest→edge latency on accept, or attributes the dedupe drop to `cache_rejected_total`
    /// `{reason}`. `ingest_us` is the measured ingest→edge-recompute span for this update.
    pub fn on_pool_update_metered(
        &mut self,
        pool: Pubkey,
        stamp: SessionStamp,
        view: PriceView,
        metrics: &DetectionMetrics,
        ingest_us: f64,
    ) -> Option<DetectionSignal> {
        match self.cache.apply_classified(pool, stamp, view) {
            ApplyOutcome::Accepted => {
                metrics.record_update();
                self.last_processed_slot = Some(
                    self.last_processed_slot
                        .map_or(stamp.slot, |s| s.max(stamp.slot)),
                );
                let signal = self
                    .graph
                    .on_event(pool, view)
                    .map(DetectionSignal::EdgeUpdated);
                metrics.record_ingest_to_edge_us(ingest_us);
                signal
            }
            ApplyOutcome::RejectedStale => {
                metrics.record_cache_reject(CacheRejectReason::StaleSlot);
                None
            }
            ApplyOutcome::RejectedDuplicate => {
                metrics.record_cache_reject(CacheRejectReason::Duplicate);
                None
            }
        }
    }

    /// Slot to resubscribe from after a disconnect (see `reconnect`).
    pub fn resubscribe_from_slot(&self) -> Option<u64> {
        reconnect::resubscribe_from_slot(self.last_processed_slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::CpmmReserves;
    use arb_types::DexKind;

    fn view(ra: u64, rb: u64, slot: u64) -> PriceView {
        PriceView {
            dex: DexKind::RaydiumCpmm,
            mint_a: Pubkey::new_from_array([1; 32]),
            mint_b: Pubkey::new_from_array([2; 32]),
            reserves: CpmmReserves::new(ra, rb, 25, 10_000),
            slot,
        }
    }

    #[test]
    fn end_to_end_two_pools_signal() {
        let mut p = DetectionPipeline::new();
        let pool1 = Pubkey::new_from_array([10; 32]);
        let pool2 = Pubkey::new_from_array([11; 32]);

        assert!(p
            .on_pool_update(pool1, SessionStamp::new(1, 100, 0), view(1_000, 1_000, 100))
            .is_none());
        let sig = p
            .on_pool_update(pool2, SessionStamp::new(1, 100, 1), view(1_000, 1_100, 100))
            .expect("dislocation signal");
        match sig {
            DetectionSignal::EdgeUpdated(u) => {
                assert_eq!(u.pools.len(), 2);
                assert!(u.best_spread_bps > 0);
            }
        }
        assert_eq!(p.resubscribe_from_slot(), Some(100));
    }

    #[test]
    fn stale_update_yields_no_signal() {
        let mut p = DetectionPipeline::new();
        let pool = Pubkey::new_from_array([10; 32]);
        assert!(p
            .on_pool_update(pool, SessionStamp::new(1, 100, 5), view(1_000, 1_000, 100))
            .is_none());
        // older slot, same session -> dropped, no signal even though it would change price.
        assert!(p
            .on_pool_update(pool, SessionStamp::new(1, 99, 9), view(1_000, 2_000, 99))
            .is_none());
        assert_eq!(
            p.cache.snapshot_pool(&pool).unwrap().reserves.reserve_b,
            1_000
        );
    }

    #[test]
    fn metered_pipeline_counts_updates_rejects_and_decode_errors() {
        let mut p = DetectionPipeline::new();
        let m = DetectionMetrics::new();
        let pool = Pubkey::new_from_array([10; 32]);

        // Accept a fresh update => updates_total + an ingest→edge sample.
        assert!(p
            .on_pool_update_metered(
                pool,
                SessionStamp::new(1, 100, 0),
                view(1_000, 1_000, 100),
                &m,
                250.0
            )
            .is_none());
        // Strictly older slot => RejectedStale.
        assert!(p
            .on_pool_update_metered(
                pool,
                SessionStamp::new(1, 99, 0),
                view(1_000, 2_000, 99),
                &m,
                250.0
            )
            .is_none());
        // Same (slot, write_version) => RejectedDuplicate.
        assert!(p
            .on_pool_update_metered(
                pool,
                SessionStamp::new(1, 100, 0),
                view(1_000, 1_000, 100),
                &m,
                250.0
            )
            .is_none());

        let s = m.snapshot();
        assert_eq!(s.updates_total, 1);
        assert_eq!(m.cache_reject_count(CacheRejectReason::StaleSlot), 1);
        assert_eq!(m.cache_reject_count(CacheRejectReason::Duplicate), 1);
        assert_eq!(s.cache_rejected_total, 2);
        assert_eq!(m.ingest_to_edge_count(), 1);

        // Per-venue decode-error increments on a bad-discriminator buffer (the REAL decoder).
        let bad = [0u8; 700];
        assert!(crate::detection::decode::decode_whirlpool(&bad).is_none());
        m.record_decode_error(DexKind::OrcaWhirlpool);
        assert_eq!(m.decode_error_count(DexKind::OrcaWhirlpool), 1);
    }
}
