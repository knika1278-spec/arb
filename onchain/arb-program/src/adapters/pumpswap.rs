//! PumpSwap AMM adapter (Fase-2 venue — where pump.fun graduations land). Constant-product,
//! so it mirrors `arb-math::cpmm` exactly off-chain. Swap is `buy`/`sell`; the direction is
//! selected by the leg's `SwapDir` at the tx-builder, which picks the right discriminator.
//!
//! ⚠️ `DISCRIMINATOR_BUY` / `DISCRIMINATOR_SELL` are the Anchor sha256 discriminators (pending M1-GATE proof) — fill from the
//! PumpSwap IDL and prove via the M1-GATE differential before mainnet.

use super::{alloc_vec::Vec, encode_with_discriminator};

/// Anchor `sha256("global:buy")[..8]` / `sha256("global:sell")[..8]` (filled 2026-06-22). MUST be
/// proven by the M1-GATE. NOTE: pumpswap buy/sell args are NOT (amount_in,min_out) — real arg
/// layout (base_amount_out / max_quote_amount_in) is a Fase-2 item (onchain-12/txbuilder-14).
pub const DISCRIMINATOR_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const DISCRIMINATOR_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Defaults to the `buy` form here; the tx-builder selects buy/sell by direction and the
/// processor forwards the chosen data. Kept single-arg to match the generic adapter shape.
pub fn encode(amount_in: u64, min_out: u64) -> Vec<u8> {
    encode_with_discriminator(&DISCRIMINATOR_BUY, amount_in, min_out)
}
