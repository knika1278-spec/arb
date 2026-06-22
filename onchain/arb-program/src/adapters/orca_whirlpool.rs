//! Orca Whirlpool adapter. Uses `swap_v2` (Token-2022-aware). Beyond amount/min-out, the
//! real Whirlpool swap also carries `sqrt_price_limit`, `amount_specified_is_input`, and
//! `a_to_b` — those are appended by the tx-builder via the account/arg layout; this skeleton
//! encodes the amount/min-out prefix only.
//!
//! ⚠️ `DISCRIMINATOR` is the Anchor sha256 discriminator (pending M1-GATE proof) — fill from the whirlpool IDL
//! (`sha256("global:swap_v2")[..8]`) and prove via the M1-GATE differential. NOTE: Orca
//! realized output comes from sqrtPriceX64 tick math and may cross ticks — the off-chain
//! mirror for Orca is `approximate()` until validated (see arb-math::venue).

use super::{alloc_vec::Vec, encode_with_discriminator};

/// Anchor `sha256("global:swap_v2")[..8]` (filled 2026-06-22). MUST still be proven by the M1-GATE.
pub const DISCRIMINATOR: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];

pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    encode_with_discriminator(&DISCRIMINATOR, amount_in, min_out)
}
