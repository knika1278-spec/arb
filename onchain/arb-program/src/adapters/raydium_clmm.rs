//! Raydium CLMM adapter (onchain-19). Unlike the CP venues, the swap instruction is
//! `swap_v2(amount, other_amount_threshold, sqrt_price_limit_x64, is_base_input)` — a DIFFERENT
//! arg layout, so it does NOT reuse the generic `[disc][amount_in][min_out]` encoder.
//!
//! For an exact-input leg: `amount = amount_in`, `other_amount_threshold = min_out` (slippage
//! floor), `sqrt_price_limit_x64 = 0` (no explicit limit → the program clamps to the
//! direction's min/max sqrt-price bound), `is_base_input = true`. The token direction (0→1 vs
//! 1→0) is determined by the account order forwarded from `remaining_accounts`, not a flag.
//!
//! ⚠️ Fase-2.5, gated by `M1-GATE-EXT`: NOT in the mainnet-safe Wave-1 trust allowlist, so a
//! leg targeting it is rejected by `verify_swap_program` until the gate is green. `DISCRIMINATOR`
//! is the Anchor `sha256("global:swap_v2")[..8]`; it MUST be proven by the M1-GATE-EXT
//! differential (real tick-array CPI) before any mainnet send.

use super::alloc_vec::Vec;

/// Anchor `sha256("global:swap_v2")[..8]` for Raydium CLMM. Pending M1-GATE-EXT proof.
pub const DISCRIMINATOR: [u8; 8] = [43, 4, 237, 11, 26, 201, 30, 98];

/// `sqrt_price_limit_x64 = 0` ⇒ the program substitutes the direction's min/max sqrt-price
/// bound (swap to the limit, bounded only by `other_amount_threshold`).
const NO_SQRT_PRICE_LIMIT: u128 = 0;

/// Encode `swap_v2(amount, other_amount_threshold, sqrt_price_limit_x64, is_base_input)` for an
/// exact-input swap: `[disc 8][amount u64][threshold u64][sqrt_price_limit u128][is_base_input u8]`
/// = 41 bytes.
pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + 8 + 8 + 16 + 1);
    v.extend_from_slice(&DISCRIMINATOR);
    v.extend_from_slice(&amount_in.to_le_bytes());
    v.extend_from_slice(&min_out.to_le_bytes());
    v.extend_from_slice(&NO_SQRT_PRICE_LIMIT.to_le_bytes());
    v.push(1u8); // is_base_input = true (exact-input)
    v
}
