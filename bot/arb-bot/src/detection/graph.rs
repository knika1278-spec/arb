//! Token-pair graph. On each pool update only the affected pair's edge is recomputed (not a
//! full re-scan, plan.md §5). When a pair holds ≥2 pools it computes the best cross-pool
//! spread; prices are normalized to the canonical mint orientation so pools listing the mints
//! in either order compare correctly.

use super::model::{canonical_pair, EdgeUpdate, PoolQuote, PriceView};
use solana_pubkey::Pubkey;
use std::collections::HashMap;

#[derive(Default)]
pub struct PairGraph {
    pairs: HashMap<(Pubkey, Pubkey), HashMap<Pubkey, PriceView>>,
}

/// Price of a pool expressed in the canonical orientation (token-`pair.1` per token-`pair.0`).
fn canonical_price(view: &PriceView, pair: (Pubkey, Pubkey)) -> f64 {
    let mid = view.mid_price();
    if (view.mint_a, view.mint_b) == pair {
        mid
    } else if mid > 0.0 {
        1.0 / mid
    } else {
        0.0
    }
}

impl PairGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/update a pool's view and recompute its pair's edge. Returns an `EdgeUpdate` when
    /// the pair has ≥2 pools (sizing decides profitability from the reserves + spread).
    pub fn on_event(&mut self, pool: Pubkey, view: PriceView) -> Option<EdgeUpdate> {
        let pair = canonical_pair(view.mint_a, view.mint_b);
        let entry = self.pairs.entry(pair).or_default();
        entry.insert(pool, view);
        if entry.len() < 2 {
            return None;
        }

        let mut pools: Vec<PoolQuote> = Vec::with_capacity(entry.len());
        let mut min_p = f64::MAX;
        let mut max_p = f64::MIN;
        let mut max_slot = 0u64;
        for (p, v) in entry.iter() {
            let price = canonical_price(v, pair);
            if price > 0.0 {
                min_p = min_p.min(price);
                max_p = max_p.max(price);
            }
            max_slot = max_slot.max(v.slot);
            pools.push(PoolQuote { pool: *p, view: *v });
        }

        let best_spread_bps = if min_p > 0.0 && max_p > min_p {
            (((max_p / min_p) - 1.0) * 10_000.0) as i64
        } else {
            0
        };

        // Deterministic order (by pool key) so downstream/tests are stable.
        pools.sort_by_key(|q| q.pool);
        Some(EdgeUpdate {
            pair,
            pools,
            best_spread_bps,
            max_slot,
        })
    }

    pub fn pool_count(&self, m1: Pubkey, m2: Pubkey) -> usize {
        self.pairs
            .get(&canonical_pair(m1, m2))
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::CpmmReserves;
    use arb_types::DexKind;

    fn pool_view(ra: u64, rb: u64, ma: u8, mb: u8) -> PriceView {
        PriceView {
            dex: DexKind::RaydiumCpmm,
            mint_a: Pubkey::new_from_array([ma; 32]),
            mint_b: Pubkey::new_from_array([mb; 32]),
            reserves: CpmmReserves::new(ra, rb, 25, 10_000),
            slot: 10,
        }
    }

    #[test]
    fn single_pool_no_edge() {
        let mut g = PairGraph::new();
        assert!(g
            .on_event(
                Pubkey::new_from_array([1; 32]),
                pool_view(1_000, 1_000, 1, 2)
            )
            .is_none());
    }

    #[test]
    fn two_pools_emit_spread() {
        let mut g = PairGraph::new();
        // Pool 1: price 1.0 ; Pool 2: price 1.1 -> ~1000 bps spread.
        let _ = g.on_event(
            Pubkey::new_from_array([1; 32]),
            pool_view(1_000, 1_000, 1, 2),
        );
        let upd = g
            .on_event(
                Pubkey::new_from_array([2; 32]),
                pool_view(1_000, 1_100, 1, 2),
            )
            .expect("edge update with 2 pools");
        assert_eq!(upd.pools.len(), 2);
        assert!(
            upd.best_spread_bps >= 900 && upd.best_spread_bps <= 1_100,
            "got {}",
            upd.best_spread_bps
        );
    }

    #[test]
    fn reversed_mint_order_normalizes() {
        let mut g = PairGraph::new();
        // Same economic price, but second pool lists mints reversed (2,1) with reciprocal
        // reserves -> canonical prices should match (≈0 spread).
        let _ = g.on_event(
            Pubkey::new_from_array([1; 32]),
            pool_view(1_000, 1_000, 1, 2),
        );
        let upd = g
            .on_event(
                Pubkey::new_from_array([2; 32]),
                pool_view(1_000, 1_000, 2, 1),
            )
            .expect("edge");
        assert_eq!(upd.best_spread_bps, 0);
    }
}
