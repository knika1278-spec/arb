//! add-1 (**BLOCKER**) — in-flight writable-account registry + one-inflight-per-pool dedupe.
//!
//! Multiple concurrent opportunities on the same hot pool take the same writable lock → they
//! serialize/collide, waste tips+fees, and defeat Jito's parallel auction (which only parallelizes
//! on disjoint locks). This registry gates the SECOND opportunity on any pool already in flight:
//! [`Executor::land`](super::facade::Executor::land) calls [`WritableAccountRegistry::try_acquire`]
//! before signing, and a contended attempt is dropped with [`DropCause::WritableContention`]. The
//! returned [`InflightGuard`] releases the locks on drop (RAII) so a panicking/early-returning
//! attempt cannot leak a permanent lock.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use solana_pubkey::Pubkey;

use super::types::DropCause;

/// Tracks which writable pool accounts are currently in flight.
#[derive(Clone, Default)]
pub struct WritableAccountRegistry {
    inflight: Arc<Mutex<HashSet<Pubkey>>>,
}

impl WritableAccountRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire all `pools` atomically, or fail if ANY is already in flight. The lock is held until
    /// the returned guard drops.
    pub fn try_acquire(&self, pools: &[Pubkey]) -> Result<InflightGuard, DropCause> {
        let mut set = self.inflight.lock().unwrap();
        if pools.iter().any(|p| set.contains(p)) {
            return Err(DropCause::WritableContention);
        }
        for p in pools {
            set.insert(*p);
        }
        Ok(InflightGuard {
            inflight: Arc::clone(&self.inflight),
            pools: pools.to_vec(),
        })
    }

    /// Number of pools currently locked.
    pub fn inflight_count(&self) -> usize {
        self.inflight.lock().unwrap().len()
    }
}

/// RAII guard: releases the held pool locks on drop.
#[derive(Debug)]
pub struct InflightGuard {
    inflight: Arc<Mutex<HashSet<Pubkey>>>,
    pools: Vec<Pubkey>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut set = self.inflight.lock().unwrap();
        for p in &self.pools {
            set.remove(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn second_opportunity_on_same_pool_is_gated() {
        let reg = WritableAccountRegistry::new();
        let _g = reg.try_acquire(&[pool(1), pool(2)]).unwrap();
        // A second opportunity touching pool 2 collides.
        assert_eq!(
            reg.try_acquire(&[pool(2), pool(3)]).unwrap_err(),
            DropCause::WritableContention
        );
        // A disjoint opportunity is fine (Jito parallel auction works on disjoint locks).
        assert!(reg.try_acquire(&[pool(4), pool(5)]).is_ok());
    }

    #[test]
    fn guard_releases_on_drop() {
        let reg = WritableAccountRegistry::new();
        {
            let _g = reg.try_acquire(&[pool(1)]).unwrap();
            assert_eq!(reg.inflight_count(), 1);
        }
        assert_eq!(reg.inflight_count(), 0);
        // Now re-acquirable.
        assert!(reg.try_acquire(&[pool(1)]).is_ok());
    }

    #[test]
    fn atomic_acquire_does_not_partially_lock_on_contention() {
        let reg = WritableAccountRegistry::new();
        let _g = reg.try_acquire(&[pool(2)]).unwrap();
        // Attempt to lock {1,2}: 2 collides => the whole acquire fails and 1 is NOT left locked.
        assert!(reg.try_acquire(&[pool(1), pool(2)]).is_err());
        assert!(reg.try_acquire(&[pool(1)]).is_ok());
    }
}
