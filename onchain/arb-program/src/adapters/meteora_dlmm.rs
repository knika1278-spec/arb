//! Meteora DLMM (LB CLMM) adapter (onchain-17). Swap instruction is
//! `swap(amount_in, min_amount_out)` — same `[disc][amount_in LE][min_out LE]` shape as the
//! Wave-1 CP venues, so it reuses [`super::encode_with_discriminator`].
//!
//! ⚠️ Fase-2.5, gated by `M1-GATE-EXT`: this venue is NOT in the mainnet-safe Wave-1 trust
//! allowlist, so a leg targeting it is rejected by `verify_swap_program` until the gate is
//! green. `DISCRIMINATOR` is the Anchor `sha256("global:swap")[..8]` (lb_clmm); it MUST be
//! proven by the M1-GATE-EXT differential before any mainnet send. The bin-array accounts the
//! swap consumes are forwarded generically from `remaining_accounts` (see `adapters::mod`).

use super::{alloc_vec::Vec, encode_with_discriminator};

/// Anchor `sha256("global:swap")[..8]` for Meteora DLMM `lb_clmm`. Pending M1-GATE-EXT proof.
pub const DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    encode_with_discriminator(&DISCRIMINATOR, amount_in, min_out)
}
