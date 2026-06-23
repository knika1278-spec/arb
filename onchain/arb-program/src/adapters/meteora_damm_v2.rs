//! Meteora DAMM v2 / CP-AMM adapter (onchain-18). Swap instruction is
//! `swap(SwapParameters { amount_in, minimum_amount_out })`; Borsh packs the two `u64`s
//! contiguously, so the wire data is the same `[disc][amount_in LE][min_out LE]` shape as the
//! Wave-1 CP venues and reuses [`super::encode_with_discriminator`].
//!
//! ⚠️ Fase-2.5, gated by `M1-GATE-EXT`: NOT in the mainnet-safe Wave-1 trust allowlist, so a
//! leg targeting it is rejected by `verify_swap_program` until the gate is green. `DISCRIMINATOR`
//! is the Anchor `sha256("global:swap")[..8]` (cp-amm); it MUST be proven by the M1-GATE-EXT
//! differential before any mainnet send.

use super::{alloc_vec::Vec, encode_with_discriminator};

/// Anchor `sha256("global:swap")[..8]` for Meteora DAMM v2 `cp-amm`. Pending M1-GATE-EXT proof.
pub const DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    encode_with_discriminator(&DISCRIMINATOR, amount_in, min_out)
}
