//! detection-12 — negative-cycle (triangle+) discovery over the live token-mint graph.
//!
//! Promotes the Fase-3 `detection-10` seam to active for Fase 2.5. Where [`super::graph::PairGraph`]
//! finds a 2-pool dislocation on ONE token pair, this finds a profitable CYCLE across several mints
//! (e.g. the ANB `base → A → B → base` triangle) by a Bellman-Ford negative-cycle search in
//! log-price space: each pool contributes two directed edges with weight `-ln(spot_rate · (1−fee))`,
//! and a negative-total-weight cycle is exactly a loop whose marginal product exceeds 1 — a
//! candidate arbitrage. The exact, gate-safe sizing of a discovered cycle is
//! [`arb_math::size_cycle`] (`sizing-15`); this stage only proposes the route.
//!
//! Discovery uses the spot (pre-impact) rate, so a found cycle is a CANDIDATE — the integer sizer
//! decides if a positive-profit size actually exists.

use super::model::PriceView;
use arb_types::DexKind;
use solana_pubkey::Pubkey;
use std::collections::HashMap;

/// Float guard so float noise can't manufacture a spurious negative cycle.
const RELAX_EPS: f64 = 1e-9;

/// One directed hop of a discovered cycle: swap `mint_in → mint_out` through `pool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CycleHop {
    pub pool: Pubkey,
    pub dex: DexKind,
    pub mint_in: Pubkey,
    pub mint_out: Pubkey,
}

/// A discovered profitable cycle: an ordered list of hops that returns to the starting mint, with
/// a marginal (post-fee, pre-impact) product `> 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArbitrageCycle {
    pub hops: Vec<CycleHop>,
}

impl ArbitrageCycle {
    pub fn len(&self) -> usize {
        self.hops.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }
    /// The mint the cycle starts and ends on.
    pub fn base_mint(&self) -> Option<Pubkey> {
        self.hops.first().map(|h| h.mint_in)
    }
}

struct Edge {
    from: usize,
    to: usize,
    weight: f64,
    hop: CycleHop,
}

fn intern(m: Pubkey, idx: &mut HashMap<Pubkey, usize>, mints: &mut Vec<Pubkey>) -> usize {
    if let Some(&i) = idx.get(&m) {
        return i;
    }
    let i = mints.len();
    mints.push(m);
    idx.insert(m, i);
    i
}

/// The post-fee spot multiplier `(fee_den − fee_num) / fee_den` for a CP pool.
fn fee_mult(reserves: &arb_math::CpmmReserves) -> f64 {
    let den = reserves.fee_denominator as f64;
    if den <= 0.0 {
        return 0.0;
    }
    (den - reserves.fee_numerator as f64) / den
}

/// Discover one profitable cycle of length `2..=max_len` among `pools` (pool key + decoded view),
/// or `None`. The two directed edges per pool are `a→b` at spot `reserve_b/reserve_a·(1−fee)` and
/// `b→a` at the reciprocal. Bellman-Ford with an all-zero init detects ANY negative cycle.
pub fn find_arbitrage_cycle(
    pools: &[(Pubkey, PriceView)],
    max_len: usize,
) -> Option<ArbitrageCycle> {
    if max_len < 2 {
        return None;
    }
    let mut idx: HashMap<Pubkey, usize> = HashMap::new();
    let mut mints: Vec<Pubkey> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    for (pool, v) in pools {
        let ra = v.reserves.reserve_a as f64;
        let rb = v.reserves.reserve_b as f64;
        if ra <= 0.0 || rb <= 0.0 {
            continue;
        }
        let g = fee_mult(&v.reserves);
        if g <= 0.0 {
            continue;
        }
        let rate_ab = (rb / ra) * g; // a in → b out
        let rate_ba = (ra / rb) * g; // b in → a out
        if rate_ab <= 0.0 || rate_ba <= 0.0 {
            continue;
        }
        let ia = intern(v.mint_a, &mut idx, &mut mints);
        let ib = intern(v.mint_b, &mut idx, &mut mints);
        edges.push(Edge {
            from: ia,
            to: ib,
            weight: -rate_ab.ln(),
            hop: CycleHop {
                pool: *pool,
                dex: v.dex,
                mint_in: v.mint_a,
                mint_out: v.mint_b,
            },
        });
        edges.push(Edge {
            from: ib,
            to: ia,
            weight: -rate_ba.ln(),
            hop: CycleHop {
                pool: *pool,
                dex: v.dex,
                mint_in: v.mint_b,
                mint_out: v.mint_a,
            },
        });
    }

    let n = mints.len();
    if n < 2 || edges.is_empty() {
        return None;
    }

    // Bellman-Ford from a virtual all-zero source (detects any negative cycle in the graph).
    let mut dist = vec![0.0f64; n];
    let mut pred: Vec<Option<usize>> = vec![None; n]; // pred[v] = edge index used to reach v
    let mut hit: Option<usize> = None;
    for _ in 0..n {
        hit = None;
        for (ei, e) in edges.iter().enumerate() {
            if dist[e.from] + e.weight < dist[e.to] - RELAX_EPS {
                dist[e.to] = dist[e.from] + e.weight;
                pred[e.to] = Some(ei);
                hit = Some(e.to);
            }
        }
        if hit.is_none() {
            break; // converged ⇒ no negative cycle
        }
    }
    let start = hit?; // still relaxing after n passes ⇒ a negative cycle is reachable from `start`

    // Step back n times to land ON the cycle, then walk it.
    let mut cur = start;
    for _ in 0..n {
        cur = edges[pred[cur]?].from;
    }
    let cycle_node = cur;
    let mut edge_path: Vec<usize> = Vec::new();
    let mut walk = cycle_node;
    loop {
        let ei = pred[walk]?;
        edge_path.push(ei);
        walk = edges[ei].from;
        if walk == cycle_node {
            break;
        }
        if edge_path.len() > n {
            return None; // safety: malformed predecessor chain
        }
    }
    edge_path.reverse();

    if edge_path.len() > max_len {
        return None; // cycle longer than the route budget (txbuilder-15 / N-leg MAX_LEGS)
    }
    let hops = edge_path
        .iter()
        .map(|&ei| edges[ei].hop)
        .collect::<Vec<_>>();
    Some(ArbitrageCycle { hops })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::CpmmReserves;

    fn mint(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }
    fn pool_key(b: u8) -> Pubkey {
        Pubkey::new_from_array([100 + b; 32])
    }
    fn view(ma: u8, mb: u8, ra: u64, rb: u64) -> PriceView {
        PriceView {
            dex: DexKind::RaydiumCpmm,
            mint_a: mint(ma),
            mint_b: mint(mb),
            reserves: CpmmReserves::new(ra, rb, 25, 10_000),
            slot: 1,
        }
    }

    #[test]
    fn finds_profitable_triangle() {
        // base(1)→A(2)→B(3)→base(1). Each leg gives ~2x at the margin ⇒ product ≫ 1.
        let pools = vec![
            (pool_key(1), view(1, 2, 1_000_000, 2_000_000)), // 1→2 spot 2.0
            (pool_key(2), view(2, 3, 1_000_000, 2_000_000)), // 2→3 spot 2.0
            (pool_key(3), view(3, 1, 1_000_000, 4_000_000)), // 3→1 spot 4.0
        ];
        let cyc = find_arbitrage_cycle(&pools, 3).expect("a profitable cycle exists");
        assert_eq!(cyc.len(), 3);
        // Hops chain head→tail and return to the base mint.
        let base = cyc.base_mint().unwrap();
        let mut cur = base;
        for h in &cyc.hops {
            assert_eq!(h.mint_in, cur);
            cur = h.mint_out;
        }
        assert_eq!(cur, base, "cycle returns to base");
    }

    #[test]
    fn balanced_pools_have_no_cycle() {
        // A symmetric triangle (all 1:1) loses to fees ⇒ no negative cycle.
        let pools = vec![
            (pool_key(1), view(1, 2, 1_000_000, 1_000_000)),
            (pool_key(2), view(2, 3, 1_000_000, 1_000_000)),
            (pool_key(3), view(3, 1, 1_000_000, 1_000_000)),
        ];
        assert!(find_arbitrage_cycle(&pools, 3).is_none());
    }

    #[test]
    fn respects_max_len_budget() {
        // A genuine 3-cycle exists, but a max_len of 2 rejects it.
        let pools = vec![
            (pool_key(1), view(1, 2, 1_000_000, 2_000_000)),
            (pool_key(2), view(2, 3, 1_000_000, 2_000_000)),
            (pool_key(3), view(3, 1, 1_000_000, 4_000_000)),
        ];
        assert!(find_arbitrage_cycle(&pools, 2).is_none());
    }

    #[test]
    fn empty_or_degenerate_input_is_none() {
        assert!(find_arbitrage_cycle(&[], 3).is_none());
        // A single zero-reserve pool yields no usable edges.
        let pools = vec![(pool_key(1), view(1, 2, 0, 1_000))];
        assert!(find_arbitrage_cycle(&pools, 3).is_none());
    }
}
