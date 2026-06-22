//! signer-5 support — lock-free counters for the signer surface.

use std::sync::atomic::{AtomicU64, Ordering};

/// Labeled outcome counters for the sign path (Prometheus-style, lock-free).
#[derive(Debug, Default)]
pub struct SignerMetrics {
    pub signatures_total: AtomicU64,
    pub shape_rejections_total: AtomicU64,
    pub cap_exceeded_total: AtomicU64,
    pub halt_blocked_total: AtomicU64,
    pub backend_error_total: AtomicU64,
}

impl SignerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn inc_signature(&self) {
        self.signatures_total.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn inc_shape_rejection(&self) {
        self.shape_rejections_total.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn inc_cap_exceeded(&self) {
        self.cap_exceeded_total.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn inc_halt_blocked(&self) {
        self.halt_blocked_total.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn inc_backend_error(&self) {
        self.backend_error_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn signatures(&self) -> u64 {
        self.signatures_total.load(Ordering::Relaxed)
    }
    pub fn shape_rejections(&self) -> u64 {
        self.shape_rejections_total.load(Ordering::Relaxed)
    }
    pub fn cap_exceeded(&self) -> u64 {
        self.cap_exceeded_total.load(Ordering::Relaxed)
    }
    pub fn halt_blocked(&self) -> u64 {
        self.halt_blocked_total.load(Ordering::Relaxed)
    }
}
