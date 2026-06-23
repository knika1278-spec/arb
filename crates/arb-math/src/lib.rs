//! `arb-math` — bit-exact integer swap math + CPMM sizing.
//!
//! This crate is the **Milestone-1 gate (`M1-GATE`) core**: the off-chain prediction of a
//! swap's output (`cpmm::quote_out`, `cpmm::RoundTrip::realized_out`) must equal the
//! on-chain CPI's realized output bit-for-bit. It is imported by BOTH the off-chain bot
//! (`bot/arb-bot`) and the on-chain program's rounding-mirror test harness so the two
//! cannot drift apart.
//!
//! All value-bearing arithmetic is checked (`clippy::arithmetic_side_effects = deny`).
//! `no_std`-capable; the only `std` dependency is the f64 closed-form sizing *hint*
//! ([`optimal`]), gated behind the default `std` feature.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

pub mod cpmm;
pub mod fees;
pub mod mul_div;
#[cfg(feature = "std")]
pub mod optimal;
pub mod policy;
pub mod rounding;
pub mod search;
pub mod u256;
pub mod venue;

// Curated public surface.
pub use cpmm::{opportunity_exists, quote_out, required_in, CpmmReserves, RoundTrip};
pub use fees::TransferFeeConfig;
pub use mul_div::{mul_div_ceil, mul_div_floor};
pub use policy::SizingPolicy;
pub use rounding::RoundDirection;
pub use search::optimal_delta_search;
pub use u256::U256;
pub use venue::{dyn_round_trip_net_out, CpmmVenue, QuoteError, QuoteIn, QuoteOut, Quoter};

#[cfg(feature = "std")]
pub use optimal::optimal_delta_general;

// Re-export the shared taxonomy so downstream code can `use arb_math::{DexKind, SwapDir}`.
pub use arb_types::{ArbError, DexKind, SwapDir};

/// Convenience: size a round-trip end-to-end — exact integer optimum, then policy fraction.
/// Returns `(sized_delta_in, predicted_final_out, predicted_profit)` or `None` if no
/// profitable size exists. This is the canonical entry the sizing module wraps.
pub fn size_round_trip(rt: &RoundTrip, policy: SizingPolicy) -> Option<(u64, u64, i128)> {
    let (optimal, _peak) = optimal_delta_search(rt)?;
    let sized = policy.apply(optimal).max(1);
    let out = rt.realized_out(sized)?;
    let profit = (out as i128).checked_sub(sized as i128)?;
    if profit > 0 {
        Some((sized, out, profit))
    } else {
        // Policy fraction can dip a marginal opportunity below zero; fall back to optimum.
        let out_opt = rt.realized_out(optimal)?;
        let prof_opt = (out_opt as i128).checked_sub(optimal as i128)?;
        if prof_opt > 0 {
            Some((optimal, out_opt, prof_opt))
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn size_round_trip_end_to_end() {
        let a = CpmmReserves::new(1_000_000, 2_000_000, 25, 10_000);
        let b = CpmmReserves::new(2_000_000, 1_100_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB);
        let (delta, out, profit) = size_round_trip(&rt, SizingPolicy::DEFAULT).expect("profit");
        assert!(profit > 0);
        assert_eq!(out as i128 - delta as i128, profit);
        // Sized delta is ~92.5% of the exact optimum.
        let (opt, _) = optimal_delta_search(&rt).unwrap();
        assert!(delta <= opt && delta * 100 >= opt * 90);
    }
}
