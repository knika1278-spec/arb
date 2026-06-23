//! Meteora DAMM v2 / CP-AMM bit-exact exact-input quoter (`sizing-13`).
//!
//! ⚠️ Despite the "CP-AMM"/"constant-product" name, DAMM v2 is **NOT** `x*y=k`. It is a
//! Uniswap-V3-style **concentrated-liquidity** AMM with a single full-range position: state is
//! `sqrt_price` (Q64.64) + `liquidity` (L) inside a fixed band `[sqrt_min_price, sqrt_max_price]`.
//! Output comes from sqrt-price deltas, never a reserves product. (A rare `CollectFeeMode::
//! Compounding` path is true `x*y=k`; mainnet pools use ConcentratedLiquidity, ported here.)
//!
//! This is a faithful port of `MeteoraAg/damm-v2` `cp-amm`
//! `liquidity_handler/concentrated_liquidity.rs::get_swap_result_from_exact_input`, verified
//! against the canonical source + live mainnet pool `E8zRkDw3UdzRc8qVWmqyQ9MLj7jhgZDHSroYud5t25A7`
//! (fase25-venue-research, 2026-06-23). Because DAMM v2 is a SINGLE continuous range, an exact-in
//! swap is a SINGLE step — no tick/bin arrays, no loop, zero on-demand accounts: the simplest of
//! the concentrated-liquidity venues. If the price would leave the band the on-chain program
//! REVERTS (`PriceRangeViolation`); exact-in never partial-fills, so this quoter DECLINES rather
//! than clamping.
//!
//! ## Rounding (must match the CPI bit-for-bit; all favor the pool)
//! * A→B (price down): `next_P = ceil(L·P / (L + amt·P))`; `out_b = floor( L·(P − next_P) >> 128 )`.
//! * B→A (price up):   `next_P = P + floor( (amt << 128) / L )`; `out_a = floor( L·(next_P − P) / (P·next_P) )`.
//! * Pool trade fee is CEIL (`get_excluded_fee_amount`), applied on the INPUT pre-swap or the
//!   OUTPUT post-swap per [`fee_on_input`].
//!
//! The total fee NUMERATOR (base scheduler + variable/volatility component, over
//! [`FEE_DENOMINATOR`]) is execution-time/clock dependent, so it is an INPUT to the quote; resolving
//! it from pool state + timestamp is the caller's job (see `detection::decode::DammV2Pool`). The
//! sqrt-price core + fee application here are bit-exact GIVEN the numerator.

use crate::mul_div::mul_div_ceil;
use crate::u256::U256;
use arb_types::SwapDir;

/// DAMM v2 fee denominator (`FEE_DENOMINATOR`): a fee numerator is taken over 1e9 (1e9 == 100%).
pub const FEE_DENOMINATOR: u64 = 1_000_000_000;
/// Global sqrt-price band floor (`MIN_SQRT_PRICE`).
pub const MIN_SQRT_PRICE: u128 = 4_295_048_016;
/// Global sqrt-price band ceiling (`MAX_SQRT_PRICE`).
pub const MAX_SQRT_PRICE: u128 = 79_226_673_521_066_979_257_578_248_091;

/// `2^64` — the Q64.64 unit; `2^128` (the Δb shift) is this squared.
const TWO_POW_64: u128 = 1u128 << 64;

/// Why a DAMM v2 quote could not be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DammV2Error {
    /// `liquidity == 0`.
    ZeroLiquidity,
    /// Fee numerator `>= FEE_DENOMINATOR` (not a valid fraction).
    InvalidFee,
    /// The swap would push the price out of `[sqrt_min_price, sqrt_max_price]` — the on-chain
    /// program reverts (exact-in does not partial-fill).
    PriceRangeViolation,
    /// A 256-bit intermediate overflowed or a result exceeded its integer width.
    Overflow,
}

/// DAMM v2 collect-fee modes (`Pool.collect_fee_mode`).
pub mod collect_fee_mode {
    pub const BOTH_TOKEN: u8 = 0;
    pub const ONLY_B: u8 = 1;
    pub const COMPOUNDING: u8 = 2;
}

/// Whether the trade fee is charged on the INPUT (pre-swap) for `(collect_fee_mode, dir)`. Per the
/// on-chain `FeeMode::get_fee_mode`, fee-on-input is true ONLY for `(OnlyB, BtoA)` and
/// `(Compounding, BtoA)`; every other combination charges the fee on the OUTPUT post-swap.
pub fn fee_on_input(collect_fee_mode: u8, dir: SwapDir) -> bool {
    matches!(
        (collect_fee_mode, dir),
        (self::collect_fee_mode::ONLY_B, SwapDir::BtoA)
            | (self::collect_fee_mode::COMPOUNDING, SwapDir::BtoA)
    )
}

/// A DAMM v2 pool's swap-relevant state. `sqrt_price` is Q64.64; `liquidity` is the single
/// full-range L; the band is `[sqrt_min_price, sqrt_max_price]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DammV2Pool {
    pub sqrt_price: u128,
    pub liquidity: u128,
    pub sqrt_min_price: u128,
    pub sqrt_max_price: u128,
}

/// The result of an exact-input quote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DammV2Quote {
    /// Tokens the trader receives (after the pool trade fee; before any Token-2022 transfer fee).
    pub amount_out: u64,
    /// Resulting sqrt price after the swap (Q64.64).
    pub next_sqrt_price: u128,
    /// Total pool trade fee taken (input-side + output-side; only one is non-zero).
    pub fee_amount: u64,
}

impl DammV2Pool {
    pub fn new(
        sqrt_price: u128,
        liquidity: u128,
        sqrt_min_price: u128,
        sqrt_max_price: u128,
    ) -> Self {
        Self {
            sqrt_price,
            liquidity,
            sqrt_min_price,
            sqrt_max_price,
        }
    }

    /// Bit-exact exact-input quote for `amount_in` in `dir`, with the resolved total
    /// `fee_numerator` (over [`FEE_DENOMINATOR`]) and the `fee_on_input` side from [`fee_on_input`].
    pub fn quote_exact_in(
        &self,
        dir: SwapDir,
        amount_in: u64,
        fee_numerator: u64,
        fee_on_input: bool,
    ) -> Result<DammV2Quote, DammV2Error> {
        if self.liquidity == 0 {
            return Err(DammV2Error::ZeroLiquidity);
        }
        if fee_numerator >= FEE_DENOMINATOR {
            return Err(DammV2Error::InvalidFee);
        }

        // Fee on input, pre-swap (CEIL): only the post-fee amount moves the price.
        let (amt_eff, fee_in) = if fee_on_input {
            let fee = trade_fee(amount_in, fee_numerator)?;
            (
                amount_in.checked_sub(fee).ok_or(DammV2Error::Overflow)?,
                fee,
            )
        } else {
            (amount_in, 0)
        };

        let p = self.sqrt_price;
        let (next, gross_out) = match dir {
            SwapDir::AtoB => {
                let next = next_sqrt_price_from_in_a(p, self.liquidity, amt_eff)?;
                if next < self.sqrt_min_price {
                    return Err(DammV2Error::PriceRangeViolation);
                }
                (next, delta_b_floor(p, next, self.liquidity)?)
            }
            SwapDir::BtoA => {
                let next = next_sqrt_price_from_in_b(p, self.liquidity, amt_eff)?;
                if next > self.sqrt_max_price {
                    return Err(DammV2Error::PriceRangeViolation);
                }
                (next, delta_a_floor(p, next, self.liquidity)?)
            }
        };

        // Fee on output, post-swap (CEIL).
        let (amount_out, fee_out) = if fee_on_input {
            (gross_out, 0)
        } else {
            let fee = trade_fee(gross_out, fee_numerator)?;
            (
                gross_out.checked_sub(fee).ok_or(DammV2Error::Overflow)?,
                fee,
            )
        };

        Ok(DammV2Quote {
            amount_out,
            next_sqrt_price: next,
            fee_amount: fee_in.checked_add(fee_out).ok_or(DammV2Error::Overflow)?,
        })
    }
}

/// `get_excluded_fee_amount`: `ceil(amount · num / FEE_DENOMINATOR)` (Rounding::Up, against trader).
fn trade_fee(amount: u64, fee_numerator: u64) -> Result<u64, DammV2Error> {
    let fee = mul_div_ceil(
        amount as u128,
        fee_numerator as u128,
        FEE_DENOMINATOR as u128,
    )
    .ok_or(DammV2Error::Overflow)?;
    u64::try_from(fee).map_err(|_| DammV2Error::Overflow)
}

/// `get_next_sqrt_price_from_amount_in_a_rounding_up`: `ceil( L·P / (L + amt·P) )`.
fn next_sqrt_price_from_in_a(p: u128, l: u128, amt: u64) -> Result<u128, DammV2Error> {
    let num = mul_u256(l, p)?;
    let amt_p = mul_u256(amt as u128, p)?;
    let den = U256::from(l)
        .checked_add(amt_p)
        .ok_or(DammV2Error::Overflow)?;
    if den.is_zero() {
        return Err(DammV2Error::Overflow);
    }
    let next = ceil_div_u256(num, den)?;
    u256_to_u128(next)
}

/// `get_next_sqrt_price_from_amount_in_b`: `P + floor( (amt << 128) / L )` (truncating div).
fn next_sqrt_price_from_in_b(p: u128, l: u128, amt: u64) -> Result<u128, DammV2Error> {
    // (amt << 128) = amt · 2^64 · 2^64.
    let amt_shl = mul_u256(amt as u128, TWO_POW_64)?
        .checked_mul(U256::from(TWO_POW_64))
        .ok_or(DammV2Error::Overflow)?;
    let delta = amt_shl
        .checked_div(U256::from(l))
        .ok_or(DammV2Error::Overflow)?;
    let delta = u256_to_u128(delta)?;
    p.checked_add(delta).ok_or(DammV2Error::Overflow)
}

/// `get_delta_amount_b_unsigned(.., Rounding::Down)` for A→B: `floor( L·(P − next) >> 128 )`.
fn delta_b_floor(p: u128, next: u128, l: u128) -> Result<u64, DammV2Error> {
    let diff = p.checked_sub(next).ok_or(DammV2Error::Overflow)?;
    let prod = mul_u256(l, diff)?;
    // >> 128 (floor) = divide by 2^64 twice.
    let out = prod
        .checked_div(U256::from(TWO_POW_64))
        .and_then(|x| x.checked_div(U256::from(TWO_POW_64)))
        .ok_or(DammV2Error::Overflow)?;
    u256_to_u64(out)
}

/// `get_delta_amount_a_unsigned(.., Rounding::Down)` for B→A: `floor( L·(next − P) / (P·next) )`.
fn delta_a_floor(p: u128, next: u128, l: u128) -> Result<u64, DammV2Error> {
    let diff = next.checked_sub(p).ok_or(DammV2Error::Overflow)?;
    let num = mul_u256(l, diff)?;
    let den = mul_u256(p, next)?;
    if den.is_zero() {
        return Err(DammV2Error::Overflow);
    }
    let out = num.checked_div(den).ok_or(DammV2Error::Overflow)?;
    u256_to_u64(out)
}

#[inline]
fn mul_u256(a: u128, b: u128) -> Result<U256, DammV2Error> {
    U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(DammV2Error::Overflow)
}

/// `ceil(num / den)` in U256 (`den` non-zero). Mirrors the on-chain `Rounding::Up` divide.
fn ceil_div_u256(num: U256, den: U256) -> Result<U256, DammV2Error> {
    let q = num.checked_div(den).ok_or(DammV2Error::Overflow)?;
    let prod = q.checked_mul(den).ok_or(DammV2Error::Overflow)?; // ≤ num
    if prod < num {
        q.checked_add(U256::one()).ok_or(DammV2Error::Overflow)
    } else {
        Ok(q)
    }
}

#[inline]
fn u256_to_u128(v: U256) -> Result<u128, DammV2Error> {
    if v > U256::from(u128::MAX) {
        Err(DammV2Error::Overflow)
    } else {
        Ok(v.as_u128())
    }
}

#[inline]
fn u256_to_u64(v: U256) -> Result<u64, DammV2Error> {
    if v > U256::from(u64::MAX) {
        Err(DammV2Error::Overflow)
    } else {
        Ok(v.as_u64())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    // Power-of-two fixtures, hand-verified, fee = 0. The A→B and B→A cases are exact inverses.
    const P1: u128 = 1u128 << 64; // sqrt price 1.0
    const P2: u128 = 1u128 << 65; // sqrt price 2.0
    const L: u128 = 1u128 << 96;

    fn pool(sqrt_price: u128) -> DammV2Pool {
        DammV2Pool::new(sqrt_price, L, MIN_SQRT_PRICE, MAX_SQRT_PRICE)
    }

    #[test]
    fn b_to_a_exact_fixture_fee_zero() {
        // B in = 2^32 at √P=2^64, L=2^96: next = 2^64 + (2^32<<128)/2^96 = 2^64 + 2^64 = 2^65;
        // out_a = floor(L·(next−P)/(P·next)) = floor(2^96·2^64 / 2^129) = 2^31.
        let q = pool(P1)
            .quote_exact_in(SwapDir::BtoA, 1u64 << 32, 0, false)
            .unwrap();
        assert_eq!(q.next_sqrt_price, P2);
        assert_eq!(q.amount_out, 1u64 << 31);
        assert_eq!(q.fee_amount, 0);
    }

    #[test]
    fn a_to_b_exact_fixture_is_inverse_of_b_to_a() {
        // A in = 2^31 at √P=2^65, L=2^96: next = ceil(L·P/(L+amt·P)) = ceil(2^161/2^97) = 2^64;
        // out_b = floor(L·(P−next)>>128) = floor(2^160/2^128) = 2^32.
        let q = pool(P2)
            .quote_exact_in(SwapDir::AtoB, 1u64 << 31, 0, false)
            .unwrap();
        assert_eq!(q.next_sqrt_price, P1);
        assert_eq!(q.amount_out, 1u64 << 32);
        assert_eq!(q.fee_amount, 0);
    }

    #[test]
    fn fee_on_input_reduces_effective_input_and_output() {
        // 1% fee on input (BtoA + OnlyB ⇒ fee on input). Output must be ≤ the fee-free output.
        let free = pool(P1)
            .quote_exact_in(SwapDir::BtoA, 1u64 << 32, 0, false)
            .unwrap();
        let fee_num = FEE_DENOMINATOR / 100; // 1%
        let charged = pool(P1)
            .quote_exact_in(SwapDir::BtoA, 1u64 << 32, fee_num, true)
            .unwrap();
        assert!(
            charged.amount_out < free.amount_out,
            "fee must reduce output"
        );
        // fee = ceil(2^32 * 1e7 / 1e9) = ceil(2^32/100).
        assert_eq!(
            charged.fee_amount,
            (((1u128 << 32) * 10_000_000).div_ceil(1_000_000_000)) as u64
        );
    }

    #[test]
    fn fee_on_output_reduces_output_only() {
        let free = pool(P1)
            .quote_exact_in(SwapDir::BtoA, 1u64 << 32, 0, false)
            .unwrap();
        let fee_num = FEE_DENOMINATOR / 100;
        let charged = pool(P1)
            .quote_exact_in(SwapDir::BtoA, 1u64 << 32, fee_num, false)
            .unwrap();
        // Same price move (fee taken from the gross output), strictly less out.
        assert_eq!(charged.next_sqrt_price, free.next_sqrt_price);
        assert!(charged.amount_out < free.amount_out);
        assert_eq!(
            charged.fee_amount,
            trade_fee(free.amount_out, fee_num).unwrap()
        );
    }

    #[test]
    fn fee_side_selection_matches_onchain_rule() {
        use collect_fee_mode::*;
        // OnlyB / Compounding charge on input only when input is B (BtoA); everything else output.
        assert!(fee_on_input(ONLY_B, SwapDir::BtoA));
        assert!(!fee_on_input(ONLY_B, SwapDir::AtoB));
        assert!(fee_on_input(COMPOUNDING, SwapDir::BtoA));
        assert!(!fee_on_input(COMPOUNDING, SwapDir::AtoB));
        assert!(!fee_on_input(BOTH_TOKEN, SwapDir::BtoA));
        assert!(!fee_on_input(BOTH_TOKEN, SwapDir::AtoB));
    }

    #[test]
    fn out_of_band_declines_rather_than_clamps() {
        // Band ceil = 2^65. B in = 2^33 at √P=2^64, L=2^96 ⇒ next = 2^64 + 2^65 = 3·2^64 > 2^65 ⇒
        // PriceRangeViolation (revert), NOT a clamped partial fill.
        let narrow = DammV2Pool::new(P1, L, MIN_SQRT_PRICE, P2);
        let r = narrow.quote_exact_in(SwapDir::BtoA, 1u64 << 33, 0, false);
        assert_eq!(r, Err(DammV2Error::PriceRangeViolation));
    }

    #[test]
    fn output_monotonic_in_input_within_band() {
        let p = pool(P1);
        let mut last = 0u64;
        for amt in [1u64 << 20, 1 << 24, 1 << 28, 1 << 30] {
            let q = p.quote_exact_in(SwapDir::BtoA, amt, 0, false).unwrap();
            assert!(
                q.amount_out >= last,
                "more input must not yield less output"
            );
            last = q.amount_out;
        }
    }

    #[test]
    fn degenerate_inputs_rejected() {
        assert_eq!(
            DammV2Pool::new(P1, 0, MIN_SQRT_PRICE, MAX_SQRT_PRICE).quote_exact_in(
                SwapDir::BtoA,
                1,
                0,
                false
            ),
            Err(DammV2Error::ZeroLiquidity)
        );
        assert_eq!(
            pool(P1).quote_exact_in(SwapDir::BtoA, 1, FEE_DENOMINATOR, true),
            Err(DammV2Error::InvalidFee)
        );
    }
}
