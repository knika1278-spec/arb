//! Idempotent pool-state cache (invariant §11). Keyed by pool pubkey. The acceptance rule is
//! the load-bearing part:
//!
//! * **Within one session** (`session_id` equal): accept only a strictly newer
//!   `(slot, write_version)` — higher slot, or same slot with higher `write_version`. This
//!   dedupes the firehose and applies only the latest write per slot.
//! * **Across sessions** (reconnect / node failover, `session_id` differs): `write_version`
//!   is INCOMPARABLE, so prefer the higher slot **unconditionally** — never drop a fresh
//!   reconnect update just because its `write_version` is smaller than the old session's.

use super::model::{PriceView, SessionStamp};
use solana_pubkey::Pubkey;
use std::collections::HashMap;

/// Returns true iff `incoming` should replace `current`.
pub fn accept_predicate(current: Option<SessionStamp>, incoming: SessionStamp) -> bool {
    match current {
        None => true,
        Some(cur) => {
            if cur.session_id == incoming.session_id {
                // Same session: strict (slot, write_version) ordering.
                incoming.slot > cur.slot
                    || (incoming.slot == cur.slot && incoming.write_version > cur.write_version)
            } else {
                // Reconnect/failover: write_version incomparable -> prefer higher-or-equal
                // slot unconditionally (take the fresh session's value).
                incoming.slot >= cur.slot
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CachedPool {
    stamp: SessionStamp,
    view: PriceView,
}

/// In-memory pool-state cache.
#[derive(Default)]
pub struct PoolStateCache {
    pools: HashMap<Pubkey, CachedPool>,
}

impl PoolStateCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an update; returns `true` if it was accepted (state changed), `false` if it was
    /// a stale/duplicate drop.
    pub fn apply(&mut self, pool: Pubkey, stamp: SessionStamp, view: PriceView) -> bool {
        let current = self.pools.get(&pool).map(|c| c.stamp);
        if accept_predicate(current, stamp) {
            self.pools.insert(pool, CachedPool { stamp, view });
            true
        } else {
            false
        }
    }

    /// Latest accepted view for a pool.
    pub fn snapshot_pool(&self, pool: &Pubkey) -> Option<PriceView> {
        self.pools.get(pool).map(|c| c.view)
    }

    pub fn len(&self) -> usize {
        self.pools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::CpmmReserves;
    use arb_types::DexKind;

    fn view(slot: u64) -> PriceView {
        PriceView {
            dex: DexKind::RaydiumCpmm,
            mint_a: Pubkey::new_from_array([1; 32]),
            mint_b: Pubkey::new_from_array([2; 32]),
            reserves: CpmmReserves::new(1_000, 1_000, 25, 10_000),
            slot,
        }
    }

    #[test]
    fn same_session_ordering() {
        // newer slot accepted; older dropped; same slot higher wv accepted; lower wv dropped.
        assert!(accept_predicate(None, SessionStamp::new(1, 100, 5)));
        assert!(accept_predicate(
            Some(SessionStamp::new(1, 100, 5)),
            SessionStamp::new(1, 101, 0)
        ));
        assert!(!accept_predicate(
            Some(SessionStamp::new(1, 100, 5)),
            SessionStamp::new(1, 99, 9)
        ));
        assert!(accept_predicate(
            Some(SessionStamp::new(1, 100, 5)),
            SessionStamp::new(1, 100, 6)
        ));
        assert!(!accept_predicate(
            Some(SessionStamp::new(1, 100, 5)),
            SessionStamp::new(1, 100, 4)
        ));
        assert!(!accept_predicate(
            Some(SessionStamp::new(1, 100, 5)),
            SessionStamp::new(1, 100, 5)
        ));
    }

    #[test]
    fn cross_session_prefers_higher_slot_ignoring_write_version() {
        // New session, LOWER write_version but HIGHER slot -> accept (the §11 reconnect rule).
        let cur = Some(SessionStamp::new(1, 100, 9_999));
        assert!(accept_predicate(cur, SessionStamp::new(2, 101, 0)));
        // New session, same slot -> accept (take the fresh session).
        assert!(accept_predicate(cur, SessionStamp::new(2, 100, 0)));
        // New session but strictly older slot -> drop.
        assert!(!accept_predicate(cur, SessionStamp::new(2, 99, 100_000)));
    }

    #[test]
    fn cache_apply_dedupes() {
        let mut c = PoolStateCache::new();
        let pool = Pubkey::new_from_array([7; 32]);
        assert!(c.apply(pool, SessionStamp::new(1, 100, 1), view(100)));
        assert!(!c.apply(pool, SessionStamp::new(1, 100, 1), view(100))); // duplicate
        assert!(c.apply(pool, SessionStamp::new(1, 101, 0), view(101))); // newer slot
        assert_eq!(c.snapshot_pool(&pool).unwrap().slot, 101);
        assert_eq!(c.len(), 1);
    }
}
