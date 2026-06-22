//! Per-venue quoting. Raydium CPMM and PumpSwap AMM are pure constant-product and share
//! the exact integer path in [`crate::cpmm`]. Orca Whirlpool is constant-product only
//! within a tick range; the bit-exact sqrtPriceX64 mirror now lives in
//! [`crate::whirlpool`] ([`crate::whirlpool::WhirlpoolPool::quote_exact_in`], `sizing-5`).
//! The [`CpmmVenue`] wrapper here still carries an Orca CP **approximation** for the advisory
//! detection/graph price view, flagged `approximate()` so callers never treat *that* path as
//! gate-exact — the gate path is `whirlpool`.

use crate::cpmm::CpmmReserves;
use arb_types::{DexKind, SwapDir};

/// A venue that can quote a single swap leg's output.
pub trait Quoter {
    fn dex(&self) -> DexKind;
    /// Floored output for `amount_in` in `dir`.
    fn quote_out(&self, dir: SwapDir, amount_in: u64) -> Option<u64>;
    /// `true` if this quote is not yet proven bit-exact against the on-chain CPI.
    fn approximate(&self) -> bool {
        false
    }
}

/// Constant-product venue carrying its `DexKind` for dispatch/labelling.
#[derive(Clone, Copy, Debug)]
pub struct CpmmVenue {
    pub dex: DexKind,
    pub reserves: CpmmReserves,
}

impl CpmmVenue {
    pub fn new(dex: DexKind, reserves: CpmmReserves) -> Self {
        Self { dex, reserves }
    }
}

impl Quoter for CpmmVenue {
    fn dex(&self) -> DexKind {
        self.dex
    }
    fn quote_out(&self, dir: SwapDir, amount_in: u64) -> Option<u64> {
        self.reserves.quote_out(dir, amount_in)
    }
    fn approximate(&self) -> bool {
        // Orca CP-form is an approximation pending the sqrt-price mirror; the other two
        // are exact constant-product.
        matches!(self.dex, DexKind::OrcaWhirlpool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raydium_exact_orca_flagged_approx() {
        let r = CpmmVenue::new(
            DexKind::RaydiumCpmm,
            CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000),
        );
        let o = CpmmVenue::new(
            DexKind::OrcaWhirlpool,
            CpmmReserves::new(1_000_000, 1_000_000, 30, 10_000),
        );
        assert!(!r.approximate());
        assert!(o.approximate());
        assert_eq!(r.quote_out(SwapDir::AtoB, 10_000), Some(9876));
    }
}
