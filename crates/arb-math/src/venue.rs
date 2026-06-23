//! Per-venue quoting (sizing-3): the day-1 [`Quoter`] abstraction over arb-math's bit-exact swap
//! math. Raydium CPMM and PumpSwap AMM are pure constant-product and share the exact integer path
//! in [`crate::cpmm`]. Orca Whirlpool is constant-product only within a tick range; its bit-exact
//! sqrtPriceX64 mirror now lives in [`crate::whirlpool`] (sizing-5). The [`CpmmVenue`] Orca path
//! here stays a CP **approximation** (flagged `approximate() == true`) for the advisory
//! detection/graph price view only — the gate path is `whirlpool`.
//!
//! [`QuoteOut`] carries BOTH the pool-facing GROSS amounts and the trader's actual balance-delta
//! NET amounts (after Token-2022 transfer fees on each side). Profit MUST be checked on the net
//! amounts — the on-chain assert measures the real balance delta, and the forward/inverse
//! transfer-fee math is non-symmetric (see [`crate::fees`]).
//!
//! The trait is OBJECT-SAFE: [`dyn_round_trip_net_out`] composes two legs through `&dyn Quoter`,
//! the heterogeneous-venue analogue of the concrete [`crate::cpmm::RoundTrip`] (which stays the
//! bit-exact M1-GATE path and is not routed through this trait).

use crate::cpmm::CpmmReserves;
use crate::fees::TransferFeeConfig;
use crate::mul_div::mul_div_floor;
use arb_types::{DexKind, SwapDir};

/// `2^64` — the Q64.64 fixed-point scale for [`Quoter::marginal_price_x64`].
const Q64: u128 = 0x1_0000_0000_0000_0000;

/// PumpSwap quotes its fees in basis points (denominator `1e4`); the total swap fee is the sum of
/// the lp + protocol + coin-creator bps (sizing-6 / mirrors `detection::decode`).
pub const PUMPSWAP_FEE_DENOMINATOR: u64 = 10_000;

/// Inputs to a single-leg exact-in quote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuoteIn {
    pub dir: SwapDir,
    /// GROSS amount the trader sends into the input transfer (the instruction amount).
    pub amount_in: u64,
}

/// Output of a single-leg quote. Exposes the pool-facing GROSS amounts AND the trader's actual
/// balance-delta NET amounts (after the per-side Token-2022 transfer fees), so callers profit-check
/// on `net_out − gross_in` rather than the pool-internal figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuoteOut {
    /// Instruction amount on the input mint (what the trader sends).
    pub gross_in: u64,
    /// Amount the POOL receives after the input mint's transfer fee — what the CP math consumes.
    pub net_in: u64,
    /// Amount the POOL emits (pool-facing output, before the output transfer fee).
    pub gross_out: u64,
    /// Amount the trader actually receives after the output mint's transfer fee (balance delta).
    pub net_out: u64,
}

/// Why a quote could not be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteError {
    /// A checked arithmetic step overflowed.
    Overflow,
    /// Degenerate pool fee config (denominator 0 or numerator ≥ denominator).
    InvalidFee,
    /// Not enough liquidity to satisfy the requested output (would drain the pool).
    InsufficientLiquidity,
    /// Orca only: the swap would cross an initialized tick, outside the in-range CP form (resolved
    /// by the sqrt-price mirror, sizing-5).
    CrossesTick,
}

/// A venue that can quote a single swap leg: exact-in output, the inverse required-input, and the
/// marginal (spot) price. Object-safe — usable as `&dyn Quoter` (see [`dyn_round_trip_net_out`]).
pub trait Quoter {
    /// Which DEX family this venue belongs to.
    fn dex(&self) -> DexKind;

    /// Exact-in quote: the pool output for `q.amount_in`, with net amounts after transfer fees.
    fn quote_exact_in(&self, q: QuoteIn) -> Result<QuoteOut, QuoteError>;

    /// Inverse: the GROSS input the trader must send so they NET exactly `amount_out` on the output
    /// mint. Ceils in the pool's favor (re-quoting the result yields ≥ `amount_out`).
    fn quote_required_in(&self, dir: SwapDir, amount_out: u64) -> Result<u64, QuoteError>;

    /// Marginal (spot) price as Q64.64 = `reserve_out / reserve_in` for `dir`. `None` if undefined
    /// (empty input reserve). Pre-fee — a hint for routing, not a fill price.
    fn marginal_price_x64(&self, dir: SwapDir) -> Option<u128>;

    /// `true` if this quote is not yet proven bit-exact against the on-chain CPI (e.g. the Orca CP
    /// approximation pending the sqrt-price mirror).
    fn approximate(&self) -> bool {
        false
    }
}

/// Constant-product venue (Raydium CPMM, PumpSwap; Orca within a tick range) carrying its reserves,
/// `DexKind`, and the optional per-side Token-2022 transfer-fee configs (`NONE` for plain SPL).
#[derive(Clone, Copy, Debug)]
pub struct CpmmVenue {
    pub dex: DexKind,
    pub reserves: CpmmReserves,
    /// Transfer-fee config of the leg's INPUT mint (`NONE` = plain SPL token).
    pub fee_in: TransferFeeConfig,
    /// Transfer-fee config of the leg's OUTPUT mint (`NONE` = plain SPL token).
    pub fee_out: TransferFeeConfig,
}

impl CpmmVenue {
    /// A plain-SPL constant-product venue (no transfer fees; `net == gross`).
    pub fn new(dex: DexKind, reserves: CpmmReserves) -> Self {
        Self {
            dex,
            reserves,
            fee_in: TransferFeeConfig::NONE,
            fee_out: TransferFeeConfig::NONE,
        }
    }

    /// A Token-2022 constant-product venue with per-side transfer-fee configs.
    pub fn with_transfer_fees(
        dex: DexKind,
        reserves: CpmmReserves,
        fee_in: TransferFeeConfig,
        fee_out: TransferFeeConfig,
    ) -> Self {
        Self {
            dex,
            reserves,
            fee_in,
            fee_out,
        }
    }

    /// PumpSwap AMM venue (sizing-6): a constant-product pool whose TOTAL swap fee is the sum of the
    /// lp + protocol + coin-creator basis points, combined ONCE and applied to the input pre-swap
    /// over the [`PUMPSWAP_FEE_DENOMINATOR`] (1e4) — exactly the on-chain order. Returns `None` if
    /// the summed fee overflows or is not a valid fraction (`>= denominator`). `base`/`quote` are
    /// the pool's two vault reserves; orient the `SwapDir` so the input side is `reserve_a`.
    pub fn pumpswap(
        reserve_base: u64,
        reserve_quote: u64,
        lp_fee_bps: u64,
        protocol_fee_bps: u64,
        coin_creator_fee_bps: u64,
    ) -> Option<Self> {
        // Combine the three fee components ONCE (checked — arb-math forbids silent overflow).
        let total_bps = lp_fee_bps
            .checked_add(protocol_fee_bps)?
            .checked_add(coin_creator_fee_bps)?;
        if total_bps >= PUMPSWAP_FEE_DENOMINATOR {
            return None;
        }
        Some(Self::new(
            DexKind::PumpSwapAmm,
            CpmmReserves::new(
                reserve_base,
                reserve_quote,
                total_bps,
                PUMPSWAP_FEE_DENOMINATOR,
            ),
        ))
    }

    #[inline]
    fn oriented_reserves(&self, dir: SwapDir) -> (u64, u64) {
        match dir {
            SwapDir::AtoB => (self.reserves.reserve_a, self.reserves.reserve_b),
            SwapDir::BtoA => (self.reserves.reserve_b, self.reserves.reserve_a),
        }
    }
}

impl Quoter for CpmmVenue {
    fn dex(&self) -> DexKind {
        self.dex
    }

    fn quote_exact_in(&self, q: QuoteIn) -> Result<QuoteOut, QuoteError> {
        // The input transfer fee is skimmed before the pool ever sees the funds.
        let fee_in = self
            .fee_in
            .calculate_fee(q.amount_in)
            .ok_or(QuoteError::Overflow)?;
        let net_in = q
            .amount_in
            .checked_sub(fee_in)
            .ok_or(QuoteError::Overflow)?;
        // Pool output on the NET input — the bit-exact constant-product path.
        let gross_out = self
            .reserves
            .quote_out(q.dir, net_in)
            .ok_or(QuoteError::InvalidFee)?;
        // The output transfer fee is skimmed on the way back to the trader.
        let fee_out = self
            .fee_out
            .calculate_fee(gross_out)
            .ok_or(QuoteError::Overflow)?;
        let net_out = gross_out.checked_sub(fee_out).ok_or(QuoteError::Overflow)?;
        Ok(QuoteOut {
            gross_in: q.amount_in,
            net_in,
            gross_out,
            net_out,
        })
    }

    fn quote_required_in(&self, dir: SwapDir, amount_out: u64) -> Result<u64, QuoteError> {
        // `amount_out` is the NET the trader wants; the pool must emit enough GROSS to cover the
        // output transfer fee, and the trader must send enough GROSS to cover the input fee.
        let gross_out = self
            .fee_out
            .calculate_pre_fee_amount(amount_out)
            .ok_or(QuoteError::Overflow)?;
        let net_in = self
            .reserves
            .required_in(dir, gross_out)
            .ok_or(QuoteError::InsufficientLiquidity)?;
        self.fee_in
            .calculate_pre_fee_amount(net_in)
            .ok_or(QuoteError::Overflow)
    }

    fn marginal_price_x64(&self, dir: SwapDir) -> Option<u128> {
        let (reserve_in, reserve_out) = self.oriented_reserves(dir);
        if reserve_in == 0 {
            return None;
        }
        mul_div_floor(reserve_out as u128, Q64, reserve_in as u128)
    }

    fn approximate(&self) -> bool {
        // Orca's CP form is an approximation pending the sqrt-price mirror; the other two are exact.
        matches!(self.dex, DexKind::OrcaWhirlpool)
    }
}

/// Compose a two-leg round-trip via DYNAMIC dispatch, proving [`Quoter`] is object-safe: feed
/// `delta_in` of base into leg A, leg A's NET output into leg B, and return leg B's NET output (the
/// trader's final balance delta). Heterogeneous-venue analogue of [`crate::cpmm::RoundTrip`]; the
/// concrete `RoundTrip` remains the bit-exact M1-GATE path.
pub fn dyn_round_trip_net_out(
    leg_a: &dyn Quoter,
    dir_a: SwapDir,
    leg_b: &dyn Quoter,
    dir_b: SwapDir,
    delta_in: u64,
) -> Result<u64, QuoteError> {
    let a = leg_a.quote_exact_in(QuoteIn {
        dir: dir_a,
        amount_in: delta_in,
    })?;
    let b = leg_b.quote_exact_in(QuoteIn {
        dir: dir_b,
        amount_in: a.net_out,
    })?;
    Ok(b.net_out)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn raydium(a: u64, b: u64) -> CpmmVenue {
        CpmmVenue::new(DexKind::RaydiumCpmm, CpmmReserves::new(a, b, 25, 10_000))
    }

    #[test]
    fn spl_quote_exact_in_net_equals_gross_and_matches_cpmm() {
        let v = raydium(1_000_000, 1_000_000);
        let out = v
            .quote_exact_in(QuoteIn {
                dir: SwapDir::AtoB,
                amount_in: 10_000,
            })
            .unwrap();
        // Same hand-computed value as the cpmm bit-exact test.
        assert_eq!(out.gross_out, 9_876);
        // Plain SPL => no transfer fee => net == gross on both sides.
        assert_eq!(out.gross_in, 10_000);
        assert_eq!(out.net_in, 10_000);
        assert_eq!(out.net_out, 9_876);
    }

    #[test]
    fn token2022_fees_make_net_distinct_from_gross() {
        // 1% input fee, 0.5% output fee (uncapped).
        let fee_in = TransferFeeConfig {
            transfer_fee_basis_points: 100,
            maximum_fee: u64::MAX,
        };
        let fee_out = TransferFeeConfig {
            transfer_fee_basis_points: 50,
            maximum_fee: u64::MAX,
        };
        let v = CpmmVenue::with_transfer_fees(
            DexKind::RaydiumCpmm,
            CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000),
            fee_in,
            fee_out,
        );
        let out = v
            .quote_exact_in(QuoteIn {
                dir: SwapDir::AtoB,
                amount_in: 10_000,
            })
            .unwrap();
        // Input fee = ceil(10000 * 100 / 10000) = 100 => pool sees 9_900 (< gross 10_000).
        assert_eq!(out.gross_in, 10_000);
        assert_eq!(out.net_in, 9_900);
        // Output is taxed too: net_out strictly less than the pool's gross_out.
        assert!(out.net_out < out.gross_out, "{out:?}");
        // The pool output is computed on the NET input, so it differs from the no-fee 9_876.
        assert!(out.gross_out < 9_876, "fee on input lowers pool output");
    }

    #[test]
    fn required_in_round_trips_within_rounding() {
        let v = raydium(5_000_000, 3_000_000);
        for amt_in in [10u64, 1_000, 50_000, 250_000] {
            let out = v
                .quote_exact_in(QuoteIn {
                    dir: SwapDir::AtoB,
                    amount_in: amt_in,
                })
                .unwrap();
            if out.net_out == 0 {
                continue;
            }
            let need = v.quote_required_in(SwapDir::AtoB, out.net_out).unwrap();
            // Ceil favors the pool: never asks for more than the original gross input...
            assert!(need <= amt_in, "need={need} amt_in={amt_in}");
            // ...and re-quoting `need` still nets at least the target out.
            let re = v
                .quote_exact_in(QuoteIn {
                    dir: SwapDir::AtoB,
                    amount_in: need,
                })
                .unwrap();
            assert!(
                re.net_out >= out.net_out,
                "re={} target={}",
                re.net_out,
                out.net_out
            );
        }
    }

    #[test]
    fn marginal_price_is_reserve_ratio_q64() {
        // reserve_out/reserve_in = 2/1 => price ≈ 2.0 in Q64.64.
        let v = raydium(1_000_000, 2_000_000);
        let p = v.marginal_price_x64(SwapDir::AtoB).unwrap();
        assert_eq!(p, 2 * Q64);
        // Reverse direction is the reciprocal ≈ 0.5.
        let p_rev = v.marginal_price_x64(SwapDir::BtoA).unwrap();
        assert_eq!(p_rev, Q64 / 2);
        // Empty input reserve => undefined.
        let empty = CpmmVenue::new(
            DexKind::RaydiumCpmm,
            CpmmReserves::new(0, 1_000, 25, 10_000),
        );
        assert_eq!(empty.marginal_price_x64(SwapDir::AtoB), None);
    }

    #[test]
    fn orca_flagged_approximate_others_exact() {
        assert!(!raydium(1_000, 1_000).approximate());
        let orca = CpmmVenue::new(
            DexKind::OrcaWhirlpool,
            CpmmReserves::new(1_000, 1_000, 30, 10_000),
        );
        assert!(orca.approximate());
    }

    #[test]
    fn dyn_round_trip_composes_via_trait_objects() {
        // Object-safety: build the legs as &dyn Quoter and chain them.
        let a = raydium(1_000_000, 2_000_000);
        let b = raydium(2_000_000, 1_100_000);
        let leg_a: &dyn Quoter = &a;
        let leg_b: &dyn Quoter = &b;
        let out =
            dyn_round_trip_net_out(leg_a, SwapDir::AtoB, leg_b, SwapDir::AtoB, 5_000).unwrap();
        // A small size on this dislocated pair returns more base than was put in (profit).
        assert!(out > 5_000, "round-trip net out {out} should exceed input");
    }

    #[test]
    fn pumpswap_combines_fee_once_and_matches_cp_both_directions() {
        // 20 lp + 5 protocol + 5 coin-creator = 30 bps total, applied ONCE pre-swap (1e4 denom).
        let v = CpmmVenue::pumpswap(1_000_000, 2_000_000, 20, 5, 5).unwrap();
        assert_eq!(v.dex(), DexKind::PumpSwapAmm);
        assert!(!v.approximate()); // pure CP, bit-exact

        // Reference: a single 30-bps constant-product application (the bit-exact cpmm path).
        let reference = CpmmReserves::new(1_000_000, 2_000_000, 30, PUMPSWAP_FEE_DENOMINATOR);
        for dir in [SwapDir::AtoB, SwapDir::BtoA] {
            let out = v
                .quote_exact_in(QuoteIn {
                    dir,
                    amount_in: 10_000,
                })
                .unwrap();
            assert_eq!(out.gross_out, reference.quote_out(dir, 10_000).unwrap());
            assert_eq!(out.net_out, out.gross_out); // plain SPL: no Token-2022 skim
        }

        // Concrete fixture (AtoB): in_after_fee = floor(10000*9970/10000)=9970,
        // out = floor(2_000_000*9970 / (1_000_000+9970)) = floor(19_940_000_000/1_009_970) = 19_743.
        let a = v
            .quote_exact_in(QuoteIn {
                dir: SwapDir::AtoB,
                amount_in: 10_000,
            })
            .unwrap();
        assert_eq!(a.gross_out, 19_743);

        // Double-applying the combined fee (a classic bug) would lower the output — guard it.
        let double_fee = CpmmReserves::new(1_000_000, 2_000_000, 60, PUMPSWAP_FEE_DENOMINATOR);
        assert!(a.gross_out > double_fee.quote_out(SwapDir::AtoB, 10_000).unwrap());
    }

    #[test]
    fn pumpswap_rejects_degenerate_total_fee() {
        // Summed fee >= denominator is not a valid fraction.
        assert!(CpmmVenue::pumpswap(1_000, 1_000, 5_000, 5_000, 1).is_none());
    }
}
