//! Detection contract types (the detection→sizing boundary). A `PriceView` is the decoded,
//! sizing-ready snapshot of one pool; an `EdgeUpdate`/`DetectionSignal` is what the graph
//! emits when a token-pair's pools dislocate.

use arb_math::CpmmReserves;
use arb_types::DexKind;
use solana_pubkey::Pubkey;

/// Idempotency key for a streamed account update. `write_version` is a per-validator,
/// per-SESSION monotonic counter — comparable ONLY within the same `session_id` (it is
/// reset/incomparable across reconnect/failover). The cache enforces that rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStamp {
    pub session_id: u64,
    pub slot: u64,
    pub write_version: u64,
}

impl SessionStamp {
    pub fn new(session_id: u64, slot: u64, write_version: u64) -> Self {
        Self {
            session_id,
            slot,
            write_version,
        }
    }
}

/// Decoded, sizing-ready view of one pool. For CPMM venues the reserves are exact; for Orca
/// Whirlpool they are an in-range constant-product approximation (flagged via `dex`, mirrors
/// `arb_math::venue::Quoter::approximate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceView {
    pub dex: DexKind,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub reserves: CpmmReserves,
    pub slot: u64,
}

impl PriceView {
    /// Mid price as token-B units per token-A (quote per base). `NaN`/`inf`-safe-ish:
    /// returns 0.0 when reserve_a is 0 (degenerate / undecodable pool).
    pub fn mid_price(&self) -> f64 {
        if self.reserves.reserve_a == 0 {
            return 0.0;
        }
        self.reserves.reserve_b as f64 / self.reserves.reserve_a as f64
    }
}

/// One pool's contribution to an edge update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolQuote {
    pub pool: Pubkey,
    pub view: PriceView,
}

/// Canonical, order-independent token-pair key (mints sorted).
pub fn canonical_pair(m1: Pubkey, m2: Pubkey) -> (Pubkey, Pubkey) {
    if m1 <= m2 {
        (m1, m2)
    } else {
        (m2, m1)
    }
}

/// Emitted when a pair with ≥2 pools shows a price dislocation worth sizing.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgeUpdate {
    pub pair: (Pubkey, Pubkey),
    pub pools: Vec<PoolQuote>,
    /// Best cross-pool spread in basis points (relative). 0 if no dislocation.
    pub best_spread_bps: i64,
    /// Highest slot among the pools feeding this update.
    pub max_slot: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DetectionSignal {
    EdgeUpdated(EdgeUpdate),
}
