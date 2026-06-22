//! Raydium CPMM adapter. Swap instruction is `swap_base_input(amount_in, minimum_amount_out)`.
//!
//! ⚠️ `DISCRIMINATOR` is the Anchor sha256 discriminator (pending M1-GATE proof) — fill from the raydium-cp-swap IDL
//! (Anchor `sha256("global:swap_base_input")[..8]`) and prove via the M1-GATE differential
//! before mainnet. Canonical CPI account order is supplied by the tx-builder; this adapter
//! only encodes the data.

use super::{alloc_vec::Vec, encode_with_discriminator};

/// Anchor `sha256("global:swap_base_input")[..8]` (filled 2026-06-22). MUST still be proven by
/// the M1-GATE differential before mainnet (necessary, not sufficient).
pub const DISCRIMINATOR: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];

pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    encode_with_discriminator(&DISCRIMINATOR, amount_in, min_out)
}
