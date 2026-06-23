//! Raydium CLMM bit-exact in-range exact-input quoter (`sizing-14`).
//!
//! A faithful port of the **DEPLOYED LEGACY** Raydium CLMM integer math
//! (`raydium-io/raydium-clmm` @ `a5a46ff`, the commit matching the on-chain 1544-byte
//! `PoolState`), for the Milestone-1 single in-range, exact-input path. Verified against the
//! canonical source + live mainnet pool `3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv`
//! (fase25-venue-research, 2026-06-23). **Do NOT port github-master's `compute_swap`** (it adds
//! fee-on-output + dynamic fee that are NOT deployed); legacy `compute_swap_step` is fee-on-input
//! only, static `trade_fee_rate`.
//!
//! Like [`crate::whirlpool`], this quotes a SINGLE constant-liquidity range and returns
//! [`RaydiumClmmError::CrossesTick`] when the (post-fee) input would reach/cross the supplied
//! `sqrt_price_limit` (the next initialized tick in the swap direction, resolved off-chain) —
//! multi-range crossing is Fase-3. Within a range the result is bit-exact.
//!
//! ## Raydium-specific details (vs the Orca port)
//! * `MAX_SQRT_PRICE_X64 = 79_226_673_521_066_979_257_578_248_091` — **differs** from Orca's
//!   `79_226_673_515_401_279_992_447_579_055`; use the Raydium value.
//! * `get_delta_amount_0_unsigned` does a **double rounding** (inner `mul_div` then an outer
//!   div, both ceil for `round_up`, both floor otherwise) — NOT a single combined divide.
//! * Fee is taken off the GROSS input FIRST, FLOOR: `amount_in_less_fee = floor(in·(1e6−f)/1e6)`;
//!   only that drives the price. `amount_in` (consumed) rounds UP, `amount_out` rounds DOWN.
//!
//! Per the established `whirlpool.rs` precedent, the wide intermediates use this crate's
//! [`U256`]; a size whose intermediate would exceed 256 bits (far larger than any realistic
//! single-range arb) returns [`RaydiumClmmError::Overflow`] rather than a wrong number.

use crate::mul_div::mul_div_floor;
use crate::u256::U256;
use arb_types::SwapDir;

/// Raydium `FEE_RATE_DENOMINATOR_VALUE`: `trade_fee_rate` is over 1e6.
pub const FEE_RATE_DENOMINATOR: u64 = 1_000_000;
/// `2^64` — the Q64.64 unit (`fixed_point_64::Q64`).
const Q64: u128 = 1u128 << 64;
/// Raydium legacy sqrt-price lower bound (tick −443636).
pub const MIN_SQRT_PRICE_X64: u128 = 4_295_048_016;
/// Raydium legacy sqrt-price upper bound (tick +443636) — note this DIFFERS from Orca's.
pub const MAX_SQRT_PRICE_X64: u128 = 79_226_673_521_066_979_257_578_248_091;

/// Why a Raydium CLMM quote could not be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaydiumClmmError {
    /// `liquidity == 0`.
    ZeroLiquidity,
    /// `trade_fee_rate >= FEE_RATE_DENOMINATOR` (would make `1e6 − f` non-positive).
    InvalidFee,
    /// `sqrt_price_limit` is on the wrong side of the current price, or out of global bounds.
    BadSqrtPriceLimit,
    /// The post-fee input would reach/cross the boundary tick — Fase-3 multi-range case.
    CrossesTick,
    /// A 256-bit intermediate overflowed or a result exceeded its integer width.
    Overflow,
    /// The resulting sqrt price left the global `[MIN, MAX]_SQRT_PRICE_X64` bounds.
    SqrtPriceOutOfBounds,
}

/// A Raydium CLMM pool's swap-relevant state. `trade_fee_rate` is over [`FEE_RATE_DENOMINATOR`]
/// (a 0.04% pool is `400`); it comes from the pool's `AmmConfig`, not the `PoolState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaydiumClmmPool {
    pub sqrt_price_x64: u128,
    pub liquidity: u128,
    pub trade_fee_rate: u32,
}

/// The result of an exact-input quote within one constant-liquidity range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaydiumClmmQuote {
    /// Floored output amount — the M1-GATE realized-output target.
    pub amount_out: u64,
    /// Gross input consumed from the order (whole input on a single in-range step).
    pub amount_in_consumed: u64,
    /// Fee taken from the gross input (`amount_in − amount_in_less_fee`).
    pub fee_amount: u64,
    /// Resulting sqrt price after the swap (Q64.64).
    pub next_sqrt_price_x64: u128,
}

impl RaydiumClmmPool {
    pub fn new(sqrt_price_x64: u128, liquidity: u128, trade_fee_rate: u32) -> Self {
        Self {
            sqrt_price_x64,
            liquidity,
            trade_fee_rate,
        }
    }

    /// Bit-exact exact-input quote for `amount_in` in `dir`, bounded by `sqrt_price_limit`.
    ///
    /// * `SwapDir::AtoB` = `zero_for_one` (token0 in, **price decreases**): `sqrt_price_limit`
    ///   must be a LOWER bound (`< current`, `>= MIN_SQRT_PRICE_X64`).
    /// * `SwapDir::BtoA` = `one_for_zero` (token1 in, **price increases**): an UPPER bound
    ///   (`> current`, `<= MAX_SQRT_PRICE_X64`).
    pub fn quote_exact_in(
        &self,
        dir: SwapDir,
        amount_in: u64,
        sqrt_price_limit: u128,
    ) -> Result<RaydiumClmmQuote, RaydiumClmmError> {
        let f = self.trade_fee_rate as u64;
        if f >= FEE_RATE_DENOMINATOR {
            return Err(RaydiumClmmError::InvalidFee);
        }
        if self.liquidity == 0 {
            return Err(RaydiumClmmError::ZeroLiquidity);
        }
        let p = self.sqrt_price_x64;
        if !(MIN_SQRT_PRICE_X64..=MAX_SQRT_PRICE_X64).contains(&p) {
            return Err(RaydiumClmmError::SqrtPriceOutOfBounds);
        }
        let zero_for_one = matches!(dir, SwapDir::AtoB);
        if zero_for_one {
            if sqrt_price_limit >= p || sqrt_price_limit < MIN_SQRT_PRICE_X64 {
                return Err(RaydiumClmmError::BadSqrtPriceLimit);
            }
        } else if sqrt_price_limit <= p || sqrt_price_limit > MAX_SQRT_PRICE_X64 {
            return Err(RaydiumClmmError::BadSqrtPriceLimit);
        }

        // Fee off the gross input FIRST, FLOOR — only the remainder moves the price.
        let net_rate = FEE_RATE_DENOMINATOR
            .checked_sub(f)
            .ok_or(RaydiumClmmError::Overflow)?;
        let amount_in_less_fee = mul_div_floor(
            amount_in as u128,
            net_rate as u128,
            FEE_RATE_DENOMINATOR as u128,
        )
        .and_then(|v| u64::try_from(v).ok())
        .ok_or(RaydiumClmmError::Overflow)?;

        // Reach-test: the (ceiled) input needed to drive the price to the boundary. `None` ⇒ the
        // delta overflowed u64 (Raydium `MaxTokenOverflow`) ⇒ boundary unreachable within u64.
        let amount_in_to_target = if zero_for_one {
            get_delta_amount_0(sqrt_price_limit, p, self.liquidity, true)
        } else {
            get_delta_amount_1(p, sqrt_price_limit, self.liquidity, true)
        };
        if let Some(t) = amount_in_to_target {
            if amount_in_less_fee >= t {
                return Err(RaydiumClmmError::CrossesTick);
            }
        }

        // After fee there may be nothing left to swap (tiny input) — zero output, price unchanged.
        if amount_in_less_fee == 0 {
            return Ok(RaydiumClmmQuote {
                amount_out: 0,
                amount_in_consumed: 0,
                fee_amount: amount_in,
                next_sqrt_price_x64: p,
            });
        }

        let next = next_sqrt_price_from_input(p, self.liquidity, amount_in_less_fee, zero_for_one)
            .ok_or(RaydiumClmmError::Overflow)?;
        if !(MIN_SQRT_PRICE_X64..=MAX_SQRT_PRICE_X64).contains(&next) {
            return Err(RaydiumClmmError::SqrtPriceOutOfBounds);
        }

        // Output is the FLOOR delta on the output token across the realized price move.
        let amount_out = if zero_for_one {
            get_delta_amount_1(next, p, self.liquidity, false)
        } else {
            get_delta_amount_0(p, next, self.liquidity, false)
        }
        .ok_or(RaydiumClmmError::Overflow)?;

        // Single in-range step: the whole gross input is consumed; fee is the floored remainder.
        let fee_amount = amount_in
            .checked_sub(amount_in_less_fee)
            .ok_or(RaydiumClmmError::Overflow)?;

        Ok(RaydiumClmmQuote {
            amount_out,
            amount_in_consumed: amount_in,
            fee_amount,
            next_sqrt_price_x64: next,
        })
    }
}

/// `(lower, upper)` of two sqrt prices.
#[inline]
fn order(p0: u128, p1: u128) -> (u128, u128) {
    if p0 <= p1 {
        (p0, p1)
    } else {
        (p1, p0)
    }
}

/// Raydium `get_delta_amount_0_unsigned`: token0 amount between two sqrt prices, with the
/// signature **double rounding** — `numerator_1 = L << 64`, `numerator_2 = √b − √a`;
/// `round_up ⇒ ceil(ceil(num1·num2/√b)/√a)`, else `floor(floor(num1·num2/√b)/√a)`. `None` on a
/// 256-bit overflow or a result `> u64::MAX` (Raydium `MaxTokenOverflow`).
fn get_delta_amount_0(p0: u128, p1: u128, liquidity: u128, round_up: bool) -> Option<u64> {
    let (a, b) = order(p0, p1);
    if a == 0 {
        return None;
    }
    let num1 = U256::from(liquidity).checked_mul(U256::from(Q64))?; // L << 64
    let num2 = U256::from(b.checked_sub(a)?);
    let res = if round_up {
        let inner = mul_div_ceil_256(num1, num2, U256::from(b))?;
        div_ceil_256(inner, U256::from(a))?
    } else {
        let inner = num1.checked_mul(num2)?.checked_div(U256::from(b))?;
        inner.checked_div(U256::from(a))?
    };
    u256_to_u64(res)
}

/// Raydium `get_delta_amount_1_unsigned`: token1 amount between two sqrt prices.
/// `round_up ⇒ ceil(L·(√b−√a)/2^64)`, else `floor(...)`. `None` on overflow / `> u64::MAX`.
fn get_delta_amount_1(p0: u128, p1: u128, liquidity: u128, round_up: bool) -> Option<u64> {
    let (a, b) = order(p0, p1);
    let prod = U256::from(liquidity).checked_mul(U256::from(b.checked_sub(a)?))?;
    let res = if round_up {
        div_ceil_256(prod, U256::from(Q64))?
    } else {
        prod.checked_div(U256::from(Q64))?
    };
    u256_to_u64(res)
}

/// Raydium `get_next_sqrt_price_from_input`: token0 (round-up) for `zero_for_one`, token1
/// (round-down) otherwise — both the `add = true` (exact-input) path.
fn next_sqrt_price_from_input(p: u128, l: u128, amount: u64, zero_for_one: bool) -> Option<u128> {
    if zero_for_one {
        next_sqrt_price_from_amount_0_round_up(p, l, amount)
    } else {
        next_sqrt_price_from_amount_1_round_down(p, l, amount)
    }
}

/// Raydium `get_next_sqrt_price_from_amount_0_rounding_up` (add): `ceil( (L<<64)·√P / ((L<<64) +
/// amount·√P) )` — the price moves DOWN, ceiled so it never overshoots.
fn next_sqrt_price_from_amount_0_round_up(p: u128, l: u128, amount: u64) -> Option<u128> {
    if amount == 0 {
        return Some(p);
    }
    let num1 = U256::from(l).checked_mul(U256::from(Q64))?; // L << 64
    let product = U256::from(amount).checked_mul(U256::from(p))?;
    let denominator = num1.checked_add(product)?; // add path
    let next = mul_div_ceil_256(num1, U256::from(p), denominator)?;
    u256_to_u128(next)
}

/// Raydium `get_next_sqrt_price_from_amount_1_rounding_down` (add): `√P + floor( (amount<<64) / L )`
/// — the price moves UP.
fn next_sqrt_price_from_amount_1_round_down(p: u128, l: u128, amount: u64) -> Option<u128> {
    if l == 0 {
        return None;
    }
    let amount_shl = U256::from(amount).checked_mul(U256::from(Q64))?; // amount << 64
    let quotient = amount_shl.checked_div(U256::from(l))?; // floor
    p.checked_add(u256_to_u128(quotient)?)
}

/// `ceil(a·b / d)` in U256 (`d` non-zero).
fn mul_div_ceil_256(a: U256, b: U256, d: U256) -> Option<U256> {
    let prod = a.checked_mul(b)?;
    let q = prod.checked_div(d)?;
    let r = prod.checked_sub(q.checked_mul(d)?)?;
    if r.is_zero() {
        Some(q)
    } else {
        q.checked_add(U256::one())
    }
}

/// `ceil(num / d)` in U256 (`d` non-zero).
fn div_ceil_256(num: U256, d: U256) -> Option<U256> {
    let q = num.checked_div(d)?;
    let prod = q.checked_mul(d)?; // ≤ num
    if prod < num {
        q.checked_add(U256::one())
    } else {
        Some(q)
    }
}

#[inline]
fn u256_to_u128(v: U256) -> Option<u128> {
    if v > U256::from(u128::MAX) {
        None
    } else {
        Some(v.as_u128())
    }
}

#[inline]
fn u256_to_u64(v: U256) -> Option<u64> {
    if v > U256::from(u64::MAX) {
        None
    } else {
        Some(v.as_u64())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const P1: u128 = 1u128 << 64; // sqrt price 1.0
    const P2: u128 = 1u128 << 65; // sqrt price 2.0
    const L: u128 = 1_000_000_000;

    #[test]
    fn legacy_max_sqrt_price_differs_from_orca() {
        // Guard against accidentally copying Orca's constant.
        assert_eq!(MAX_SQRT_PRICE_X64, 79_226_673_521_066_979_257_578_248_091);
        assert_ne!(MAX_SQRT_PRICE_X64, 79_226_673_515_401_279_992_447_579_055);
        assert_eq!(MIN_SQRT_PRICE_X64, 4_295_048_016);
    }

    #[test]
    fn one_for_zero_fee_zero_matches_hand_value() {
        // token1 in = L at √P=2^64, limit far above ⇒ next = 2^64 + (L<<64)/L = 2^65;
        // out token0 = floor(L·(√b−√a)·2^64 /√b /√a) = floor(L·2^64·2^64/2^65/2^64) = L/2.
        let pool = RaydiumClmmPool::new(P1, L, 0);
        let q = pool
            .quote_exact_in(SwapDir::BtoA, L as u64, 1u128 << 66)
            .unwrap();
        assert_eq!(q.next_sqrt_price_x64, P2);
        assert_eq!(q.amount_out, (L / 2) as u64);
        assert_eq!(q.fee_amount, 0);
        assert_eq!(q.amount_in_consumed, L as u64);
    }

    #[test]
    fn zero_for_one_is_inverse_of_one_for_zero() {
        // token0 in = L/2 at √P=2^65, limit far below ⇒ next = 2^64; out token1 = L.
        let pool = RaydiumClmmPool::new(P2, L, 0);
        let q = pool
            .quote_exact_in(SwapDir::AtoB, (L / 2) as u64, 1u128 << 63)
            .unwrap();
        assert_eq!(q.next_sqrt_price_x64, P1);
        assert_eq!(q.amount_out, L as u64);
        assert_eq!(q.fee_amount, 0);
    }

    #[test]
    fn fee_reduces_output_below_fee_free() {
        let free = RaydiumClmmPool::new(P1, L, 0);
        let charged = RaydiumClmmPool::new(P1, L, 3000); // 0.3% over 1e6
        let limit = 1u128 << 66;
        let qf = free.quote_exact_in(SwapDir::BtoA, L as u64, limit).unwrap();
        let qc = charged
            .quote_exact_in(SwapDir::BtoA, L as u64, limit)
            .unwrap();
        assert!(qc.amount_out < qf.amount_out, "fee must reduce output");
        assert!(qc.fee_amount > 0);
        // amount_in_less_fee = floor(L·(1e6−3000)/1e6).
        assert_eq!(qc.fee_amount, L as u64 - (L * 997_000 / 1_000_000) as u64);
    }

    #[test]
    fn crosses_tick_when_limit_too_close() {
        // A limit just above current ⇒ the post-fee input reaches it ⇒ CrossesTick.
        let pool = RaydiumClmmPool::new(P1, L, 0);
        let limit = P1 + (1u128 << 30);
        assert_eq!(
            pool.quote_exact_in(SwapDir::BtoA, L as u64, limit),
            Err(RaydiumClmmError::CrossesTick)
        );
    }

    #[test]
    fn rejects_limit_on_wrong_side_and_degenerate() {
        let pool = RaydiumClmmPool::new(P2, L, 0);
        assert_eq!(
            pool.quote_exact_in(SwapDir::BtoA, 10, P1), // b->a needs UPPER limit
            Err(RaydiumClmmError::BadSqrtPriceLimit)
        );
        assert_eq!(
            pool.quote_exact_in(SwapDir::AtoB, 10, 1u128 << 66), // a->b needs LOWER limit
            Err(RaydiumClmmError::BadSqrtPriceLimit)
        );
        assert_eq!(
            RaydiumClmmPool::new(P1, 0, 0).quote_exact_in(SwapDir::BtoA, 1, P2),
            Err(RaydiumClmmError::ZeroLiquidity)
        );
        assert_eq!(
            RaydiumClmmPool::new(P1, L, FEE_RATE_DENOMINATOR as u32).quote_exact_in(
                SwapDir::BtoA,
                1,
                P2
            ),
            Err(RaydiumClmmError::InvalidFee)
        );
    }

    #[test]
    fn output_monotonic_in_input_within_range() {
        let pool = RaydiumClmmPool::new(P1, L, 3000);
        let limit = 1u128 << 66;
        let mut last = 0u64;
        for amt in [1_000u64, 10_000, 100_000, 1_000_000, 5_000_000] {
            let q = pool.quote_exact_in(SwapDir::BtoA, amt, limit).unwrap();
            assert!(
                q.amount_out >= last,
                "more input must not yield less output"
            );
            last = q.amount_out;
        }
    }

    #[test]
    fn tiny_input_after_fee_is_zero_output_not_error() {
        // 1 unit at 0.3% ⇒ floor(1·997000/1e6) = 0 ⇒ zero output, price unchanged.
        let pool = RaydiumClmmPool::new(P1, L, 3000);
        let q = pool.quote_exact_in(SwapDir::BtoA, 1, 1u128 << 66).unwrap();
        assert_eq!(q.amount_out, 0);
        assert_eq!(q.amount_in_consumed, 0);
        assert_eq!(q.fee_amount, 1);
        assert_eq!(q.next_sqrt_price_x64, P1);
    }
}
