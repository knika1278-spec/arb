//! Orca Whirlpool bit-exact in-range swap quoter (`sizing-5`).
//!
//! This is a faithful port of the Orca Whirlpool on-chain integer math
//! (`orca-so/whirlpools`, `programs/whirlpool/src/math/{swap_math,token_math}.rs`) for the
//! Milestone-1 **single constant-liquidity range, exact-input** path. Bit-exactness is the
//! `M1-GATE` requirement: this quoter's `amount_out` MUST equal the realized output of a real
//! `swap_v2` CPI for the same `(sqrt_price, liquidity, fee_rate, amount_in, direction)`, *as
//! long as the swap does not cross an initialized tick*. When the (post-fee) input would push
//! the price to/past the supplied swap-direction boundary `sqrt_price_limit` (the next
//! initialized tick, resolved off-chain by [`txbuilder::whirlpool`](../../bot/arb-bot) /
//! `add-6`), we return [`WhirlpoolError::CrossesTick`] rather than a wrong number — multi-range
//! crossing is a Fase-3 case.
//!
//! Unlike [`crate::cpmm`], Whirlpool math is **not** constant-product across the whole curve:
//! realized amounts come from the Q64.64 `sqrt_price` + `liquidity`, with **Floor on the output
//! side and Ceil on the input side, per direction**. No code is shared with `cpmm` (plan §11).
//!
//! ## Sources mirrored (Orca canonical, verified 2026-06-22)
//! * `compute_swap` — fee taken on the input (floor), reach-target test, `next_sqrt_price`,
//!   `fee_amount = gross − amount_in` for an in-range step.
//! * `get_next_sqrt_price_from_a_round_up` / `get_next_sqrt_price_from_b_round_down`.
//! * `try_get_amount_delta_a` / `try_get_amount_delta_b` (round-up vs round-down).
//!
//! ## Constants (Orca)
//! `FEE_RATE_MUL_VALUE = 1_000_000`, `MAX_FEE_RATE = 60_000`, `Q64_RESOLUTION = 64`,
//! `MIN_SQRT_PRICE_X64 = 4_295_048_016`, `MAX_SQRT_PRICE_X64 = 79_226_673_515_401_279_992_447_579_055`.
//!
//! ## Implementation notes
//! Orca uses a bespoke `U256Muldiv`; we route the same integer values through this crate's
//! [`crate::u256::U256`]. The 256-bit overflow behaviour is preserved: Orca's
//! `checked_shift_word_left` (left-shift by one 64-bit word) is `checked_mul(2^64)`, which is
//! `None` exactly when the value `≥ 2^192` — identical to Orca returning `MultiplicationOverflow`.

use crate::u256::U256;
use arb_types::SwapDir;

/// Orca fee-rate denominator (`fee_rate` is out of 1_000_000).
const FEE_RATE_MUL_VALUE: u128 = 1_000_000;
/// Maximum Whirlpool `fee_rate` (6%). Orca rejects pools above this.
const MAX_FEE_RATE: u16 = 60_000;
/// `2^64` — the Q64.64 fixed-point unit (Orca `TO_Q64`).
const TO_Q64: u128 = 1u128 << 64;
/// Low 64 bits mask — the Q64.64 fractional part (Orca `Q64_MASK`).
const Q64_MASK: u128 = 0xFFFF_FFFF_FFFF_FFFF;
/// Orca sqrt-price lower bound (tick −443636).
const MIN_SQRT_PRICE_X64: u128 = 4_295_048_016;
/// Orca sqrt-price upper bound (tick +443636).
const MAX_SQRT_PRICE_X64: u128 = 79_226_673_515_401_279_992_447_579_055;

/// Why a Whirlpool quote could not be produced. Mirrors the relevant Orca `ErrorCode`s, plus the
/// M1 `CrossesTick` boundary case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhirlpoolError {
    /// `fee_rate` exceeds `MAX_FEE_RATE` (invalid pool).
    FeeRateTooHigh,
    /// `liquidity == 0` — no active liquidity to swap against.
    ZeroLiquidity,
    /// `sqrt_price_limit` is on the wrong side of the current price, or out of global bounds.
    BadSqrtPriceLimit,
    /// The post-fee input would reach/cross the supplied boundary tick — Fase-3 multi-range case.
    CrossesTick,
    /// A 256-bit intermediate overflowed, or a result exceeded its integer width
    /// (mirrors Orca `MultiplicationOverflow` / `TokenMaxExceeded`).
    Overflow,
    /// The resulting sqrt price left the global `[MIN, MAX]_SQRT_PRICE_X64` bounds.
    SqrtPriceOutOfBounds,
}

/// A Whirlpool pool's swap-relevant state. `fee_rate` is out of `1_000_000` (e.g. a 0.3% pool is
/// `3000`). `sqrt_price_x64` is Q64.64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WhirlpoolPool {
    pub sqrt_price_x64: u128,
    pub liquidity: u128,
    pub fee_rate: u16,
}

/// The result of an exact-input quote within one constant-liquidity range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WhirlpoolQuote {
    /// Floored output amount — the `M1-GATE` realized-output target.
    pub amount_out: u64,
    /// Net input consumed by the swap (ceiled; `≤ amount_in`, the rest is fee).
    pub amount_in_consumed: u64,
    /// Fee taken from the gross input (`amount_in − amount_in_consumed` for an in-range step).
    pub fee_amount: u64,
    /// Resulting sqrt price after the swap (Q64.64).
    pub next_sqrt_price_x64: u128,
}

impl WhirlpoolPool {
    pub fn new(sqrt_price_x64: u128, liquidity: u128, fee_rate: u16) -> Self {
        Self {
            sqrt_price_x64,
            liquidity,
            fee_rate,
        }
    }

    /// Bit-exact exact-input quote for `amount_in` in `dir`, bounded by `sqrt_price_limit` (the
    /// next initialized tick's sqrt price in the swap direction, resolved off-chain).
    ///
    /// * `SwapDir::AtoB` (a→b): token A in, **price decreases** — `sqrt_price_limit` must be a
    ///   LOWER bound (`< current`, `≥ MIN_SQRT_PRICE_X64`).
    /// * `SwapDir::BtoA` (b→a): token B in, **price increases** — `sqrt_price_limit` must be an
    ///   UPPER bound (`> current`, `≤ MAX_SQRT_PRICE_X64`).
    ///
    /// Returns [`WhirlpoolError::CrossesTick`] if the swap would reach/cross the boundary.
    pub fn quote_exact_in(
        &self,
        dir: SwapDir,
        amount_in: u64,
        sqrt_price_limit: u128,
    ) -> Result<WhirlpoolQuote, WhirlpoolError> {
        if self.fee_rate > MAX_FEE_RATE {
            return Err(WhirlpoolError::FeeRateTooHigh);
        }
        if self.liquidity == 0 {
            return Err(WhirlpoolError::ZeroLiquidity);
        }
        let current = self.sqrt_price_x64;
        if !(MIN_SQRT_PRICE_X64..=MAX_SQRT_PRICE_X64).contains(&current) {
            return Err(WhirlpoolError::SqrtPriceOutOfBounds);
        }
        let a_to_b = matches!(dir, SwapDir::AtoB);
        // The limit must be on the correct side of the current price and within global bounds.
        if a_to_b {
            if sqrt_price_limit >= current || sqrt_price_limit < MIN_SQRT_PRICE_X64 {
                return Err(WhirlpoolError::BadSqrtPriceLimit);
            }
        } else if sqrt_price_limit <= current || sqrt_price_limit > MAX_SQRT_PRICE_X64 {
            return Err(WhirlpoolError::BadSqrtPriceLimit);
        }

        // Orca: fee taken from the input first, floored — `amount_calc = floor(in·(1e6−f)/1e6)`.
        let net_rate = FEE_RATE_MUL_VALUE
            .checked_sub(self.fee_rate as u128)
            .ok_or(WhirlpoolError::Overflow)?;
        let amount_calc_u128 =
            crate::mul_div::mul_div_floor(amount_in as u128, net_rate, FEE_RATE_MUL_VALUE)
                .ok_or(WhirlpoolError::Overflow)?;
        let amount_calc = u64::try_from(amount_calc_u128).map_err(|_| WhirlpoolError::Overflow)?;

        // Cross test: the (ceiled) input needed to *reach* the boundary. If the post-fee input is
        // at least that, the swap would consume the whole range and continue past the tick.
        let initial_fixed =
            try_amount_fixed_delta(current, sqrt_price_limit, self.liquidity, a_to_b)?;
        if initial_fixed.lte(amount_calc) {
            return Err(WhirlpoolError::CrossesTick);
        }

        // After fee there may be nothing left to swap (tiny input) — zero output, price unchanged.
        if amount_calc == 0 {
            return Ok(WhirlpoolQuote {
                amount_out: 0,
                amount_in_consumed: 0,
                fee_amount: amount_in,
                next_sqrt_price_x64: current,
            });
        }

        let next = next_sqrt_price(current, self.liquidity, amount_calc, a_to_b)?;
        // Output is the *unfixed* delta, floored; consumed input is the *fixed* delta, ceiled.
        let amount_out = amount_unfixed_delta(current, next, self.liquidity, a_to_b)?;
        let amount_in_consumed = amount_fixed_delta(current, next, self.liquidity, a_to_b)?;
        // In-range step (Orca `!is_max_swap`): the whole gross input is spent; fee is the remainder.
        let fee_amount = amount_in
            .checked_sub(amount_in_consumed)
            .ok_or(WhirlpoolError::Overflow)?;

        Ok(WhirlpoolQuote {
            amount_out,
            amount_in_consumed,
            fee_amount,
            next_sqrt_price_x64: next,
        })
    }
}

/// Mirror of Orca `AmountDeltaU64`: a token-amount delta that may exceed `u64`.
#[derive(Clone, Copy, Debug)]
enum AmountDelta {
    Valid(u64),
    ExceedsMax,
}

impl AmountDelta {
    /// `Valid(v) && v ≤ other`; `ExceedsMax` is never `≤` anything (Orca semantics).
    fn lte(self, other: u64) -> bool {
        matches!(self, AmountDelta::Valid(v) if v <= other)
    }
}

/// `(lower, upper)` of two sqrt prices (Orca `increasing_price_order`).
#[inline]
fn order(p0: u128, p1: u128) -> (u128, u128) {
    if p0 <= p1 {
        (p0, p1)
    } else {
        (p1, p0)
    }
}

/// Orca `checked_shift_word_left`: left-shift by one 64-bit word = `· 2^64`. `None` iff the value
/// is `≥ 2^192` (would overflow 256 bits), matching Orca returning `MultiplicationOverflow`.
#[inline]
fn shl_word(v: U256) -> Option<U256> {
    v.checked_mul(U256::from(TO_Q64))
}

/// `floor(num / den)` and the remainder, in U256, using only `checked_*` ops (the crate denies
/// raw arithmetic). `den` must be non-zero (callers guard it).
#[inline]
fn div_rem(num: U256, den: U256) -> Option<(U256, U256)> {
    let q = num.checked_div(den)?;
    let prod = q.checked_mul(den)?; // ≤ num, never overflows
    let r = num.checked_sub(prod)?; // ≥ 0
    Some((q, r))
}

/// Orca `try_get_amount_delta_a`: token-A amount between two sqrt prices.
/// `numerator = (L · Δ√P) · 2^64`, `denominator = √P_upper · √P_lower`, `round_up ⇒ +1` on a
/// non-zero remainder. Result `> u64::MAX` ⇒ `ExceedsMax`.
fn try_amount_delta_a(
    p0: u128,
    p1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<AmountDelta, WhirlpoolError> {
    let (lo, hi) = order(p0, p1);
    let diff = hi.checked_sub(lo).ok_or(WhirlpoolError::Overflow)?;
    let ls = U256::from(liquidity)
        .checked_mul(U256::from(diff))
        .ok_or(WhirlpoolError::Overflow)?;
    let numerator = shl_word(ls).ok_or(WhirlpoolError::Overflow)?;
    let denominator = U256::from(hi)
        .checked_mul(U256::from(lo))
        .ok_or(WhirlpoolError::Overflow)?;
    if denominator.is_zero() {
        // A zero sqrt price is not a valid pool state; no amount moves.
        return Ok(AmountDelta::Valid(0));
    }
    let (quotient, remainder) = div_rem(numerator, denominator).ok_or(WhirlpoolError::Overflow)?;
    let value = if round_up && !remainder.is_zero() {
        quotient
            .checked_add(U256::one())
            .ok_or(WhirlpoolError::Overflow)?
    } else {
        quotient
    };
    if value > U256::from(u64::MAX) {
        Ok(AmountDelta::ExceedsMax)
    } else {
        Ok(AmountDelta::Valid(value.as_u64()))
    }
}

/// Orca `try_get_amount_delta_b`: token-B amount between two sqrt prices.
/// `p = L · Δ√P`, `result = p >> 64`, `round_up ⇒ +1` iff the low 64 bits are non-zero.
fn try_amount_delta_b(
    p0: u128,
    p1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<AmountDelta, WhirlpoolError> {
    let (lo, hi) = order(p0, p1);
    let n1 = hi.checked_sub(lo).ok_or(WhirlpoolError::Overflow)?;
    if liquidity == 0 || n1 == 0 {
        return Ok(AmountDelta::Valid(0));
    }
    match liquidity.checked_mul(n1) {
        Some(p) => {
            // p < 2^128 ⇒ p >> 64 < 2^64, so this `try_from` cannot fail.
            let shifted = p.checked_div(TO_Q64).ok_or(WhirlpoolError::Overflow)?;
            let result = u64::try_from(shifted).map_err(|_| WhirlpoolError::Overflow)?;
            let should_round = round_up && (p & Q64_MASK) > 0;
            if should_round && result == u64::MAX {
                return Ok(AmountDelta::ExceedsMax);
            }
            let value = if should_round {
                result.checked_add(1).ok_or(WhirlpoolError::Overflow)?
            } else {
                result
            };
            Ok(AmountDelta::Valid(value))
        }
        None => Ok(AmountDelta::ExceedsMax),
    }
}

/// Erroring form of [`try_amount_delta_a`] (Orca `get_amount_delta_a`): `ExceedsMax ⇒ Err`.
fn amount_delta_a(
    p0: u128,
    p1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<u64, WhirlpoolError> {
    match try_amount_delta_a(p0, p1, liquidity, round_up)? {
        AmountDelta::Valid(v) => Ok(v),
        AmountDelta::ExceedsMax => Err(WhirlpoolError::Overflow),
    }
}

/// Erroring form of [`try_amount_delta_b`] (Orca `get_amount_delta_b`): `ExceedsMax ⇒ Err`.
fn amount_delta_b(
    p0: u128,
    p1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<u64, WhirlpoolError> {
    match try_amount_delta_b(p0, p1, liquidity, round_up)? {
        AmountDelta::Valid(v) => Ok(v),
        AmountDelta::ExceedsMax => Err(WhirlpoolError::Overflow),
    }
}

/// Orca `get_next_sqrt_price_from_a_round_up` for the exact-input path (`amount_specified_is_input
/// = true`): `√P' = ceil( (L·√P·2^64) / (L·2^64 + √P·amount) )`.
fn next_sqrt_price_from_a_round_up(
    sqrt_price: u128,
    liquidity: u128,
    amount: u64,
) -> Result<u128, WhirlpoolError> {
    if amount == 0 {
        return Ok(sqrt_price);
    }
    let product = U256::from(sqrt_price)
        .checked_mul(U256::from(amount))
        .ok_or(WhirlpoolError::Overflow)?;
    let ls = U256::from(liquidity)
        .checked_mul(U256::from(sqrt_price))
        .ok_or(WhirlpoolError::Overflow)?;
    let numerator = shl_word(ls).ok_or(WhirlpoolError::Overflow)?;
    let liquidity_shl = U256::from(liquidity)
        .checked_mul(U256::from(TO_Q64))
        .ok_or(WhirlpoolError::Overflow)?;
    // Exact-input ⇒ denominator grows (price moves down); Orca skips the div-by-zero check here.
    let denominator = liquidity_shl
        .checked_add(product)
        .ok_or(WhirlpoolError::Overflow)?;
    if denominator.is_zero() {
        return Err(WhirlpoolError::Overflow);
    }
    let (quotient, remainder) = div_rem(numerator, denominator).ok_or(WhirlpoolError::Overflow)?;
    // `div_round_up_if(.., true)` — always round up for the a-path.
    let price = if !remainder.is_zero() {
        quotient
            .checked_add(U256::one())
            .ok_or(WhirlpoolError::Overflow)?
    } else {
        quotient
    };
    if price > U256::from(u128::MAX) {
        return Err(WhirlpoolError::Overflow);
    }
    let price = price.as_u128();
    if !(MIN_SQRT_PRICE_X64..=MAX_SQRT_PRICE_X64).contains(&price) {
        return Err(WhirlpoolError::SqrtPriceOutOfBounds);
    }
    Ok(price)
}

/// Orca `get_next_sqrt_price_from_b_round_down` for the exact-input path: `√P' = √P + floor(
/// (amount·2^64) / L )`.
fn next_sqrt_price_from_b_round_down(
    sqrt_price: u128,
    liquidity: u128,
    amount: u64,
) -> Result<u128, WhirlpoolError> {
    if liquidity == 0 {
        return Err(WhirlpoolError::ZeroLiquidity);
    }
    // (amount: u64) · 2^64 < 2^128, so this cannot overflow u128.
    let amount_x64 = (amount as u128)
        .checked_mul(TO_Q64)
        .ok_or(WhirlpoolError::Overflow)?;
    // Exact-input ⇒ `div_round_up_if(.., !input=false)` ⇒ floor.
    let delta = amount_x64
        .checked_div(liquidity)
        .ok_or(WhirlpoolError::Overflow)?;
    sqrt_price
        .checked_add(delta)
        .ok_or(WhirlpoolError::SqrtPriceOutOfBounds)
}

/// Orca `get_next_sqrt_price` for exact input: a-path when `a_to_b`, b-path otherwise
/// (`amount_specified_is_input == a_to_b`).
fn next_sqrt_price(
    sqrt_price: u128,
    liquidity: u128,
    amount: u64,
    a_to_b: bool,
) -> Result<u128, WhirlpoolError> {
    if a_to_b {
        next_sqrt_price_from_a_round_up(sqrt_price, liquidity, amount)
    } else {
        next_sqrt_price_from_b_round_down(sqrt_price, liquidity, amount)
    }
}

/// Orca `try_get_amount_fixed_delta` for exact input (round-up the fixed/input side): token-A for
/// `a_to_b`, token-B otherwise.
fn try_amount_fixed_delta(
    current: u128,
    target: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Result<AmountDelta, WhirlpoolError> {
    if a_to_b {
        try_amount_delta_a(current, target, liquidity, true)
    } else {
        try_amount_delta_b(current, target, liquidity, true)
    }
}

/// Orca `get_amount_fixed_delta` for exact input — the consumed input amount (ceiled).
fn amount_fixed_delta(
    current: u128,
    next: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Result<u64, WhirlpoolError> {
    if a_to_b {
        amount_delta_a(current, next, liquidity, true)
    } else {
        amount_delta_b(current, next, liquidity, true)
    }
}

/// Orca `get_amount_unfixed_delta` for exact input — the produced output amount (floored): token-B
/// for `a_to_b`, token-A otherwise.
fn amount_unfixed_delta(
    current: u128,
    next: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Result<u64, WhirlpoolError> {
    if a_to_b {
        amount_delta_b(current, next, liquidity, false)
    } else {
        amount_delta_a(current, next, liquidity, false)
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    // Q64.64 unit and small multiples, chosen so the integer math is exactly hand-verifiable.
    const P1: u128 = 1u128 << 64; // price 1.0
    const P2: u128 = 1u128 << 65; // price 4.0 (√P = 2.0)
    const L: u128 = 1_000_000_000;

    // --- amount-delta primitives (power-of-two fixtures, hand-computed) ---

    #[test]
    fn delta_b_is_l_times_delta_sqrt_over_2pow64() {
        // ΔP = P2 − P1 = 2^64 ⇒ amount_b = floor(L · 2^64 / 2^64) = L, remainder 0.
        assert_eq!(
            try_amount_delta_b(P1, P2, L, false).map(unwrap_valid),
            Ok(L as u64)
        );
        // No fractional part ⇒ round_up does not add 1.
        assert_eq!(
            try_amount_delta_b(P1, P2, L, true).map(unwrap_valid),
            Ok(L as u64)
        );
    }

    #[test]
    fn delta_a_is_half_l_for_doubling_sqrt_price() {
        // amount_a = L·ΔP·2^64 / (P_up·P_lo) = L·2^64·2^64 / (2^65·2^64) = L/2, remainder 0.
        assert_eq!(
            try_amount_delta_a(P1, P2, L, false).map(unwrap_valid),
            Ok((L / 2) as u64)
        );
        assert_eq!(
            try_amount_delta_a(P1, P2, L, true).map(unwrap_valid),
            Ok((L / 2) as u64)
        );
    }

    #[test]
    fn delta_a_round_up_adds_one_on_remainder() {
        // Pick reserves with a non-zero remainder so Floor and Ceil differ by exactly 1.
        let lo = P1;
        let hi = P1 + 3; // tiny ΔP ⇒ amount_a rounds to 0 (floor) / 1 (ceil)
        let floor = unwrap_valid(try_amount_delta_a(lo, hi, L, false).unwrap());
        let ceil = unwrap_valid(try_amount_delta_a(lo, hi, L, true).unwrap());
        assert_eq!(
            ceil,
            floor + 1,
            "ceil must exceed floor by 1 on a remainder"
        );
    }

    // --- next-sqrt-price primitives ---

    #[test]
    fn next_sqrt_price_b_input_moves_up() {
        // Put L of token B in at √P=P1 ⇒ ΔP = L·2^64 / L = 2^64 ⇒ next = P1 + 2^64 = P2.
        assert_eq!(next_sqrt_price_from_b_round_down(P1, L, L as u64), Ok(P2));
    }

    #[test]
    fn next_sqrt_price_a_input_moves_down() {
        // Put L/2 of token A in at √P=P2 ⇒ next = ceil(L·P2·2^64 / (L·2^64 + P2·L/2)).
        // = ceil(L·2^65·2^64 / (L·2^64 + 2^65·L/2)) = ceil(2^129 / (2^64 + 2^64)) ·(L/L)
        // denominator = L·2^64 + L·2^64 = 2·L·2^64 ⇒ next = 2^129·L / (2·L·2^64) = 2^64 = P1.
        assert_eq!(
            next_sqrt_price_from_a_round_up(P2, L, (L / 2) as u64),
            Ok(P1)
        );
    }

    #[test]
    fn next_sqrt_price_a_zero_amount_is_identity() {
        assert_eq!(next_sqrt_price_from_a_round_up(P2, L, 0), Ok(P2));
    }

    // --- exact-input quotes (hand-verified, fee = 0) ---

    #[test]
    fn quote_b_to_a_fee_zero_matches_hand_value() {
        // B in = L at √P=P1, limit well above P2 ⇒ no cross. Output token A = delta_a floored.
        let pool = WhirlpoolPool::new(P1, L, 0);
        let limit = 1u128 << 66; // far above
        let q = pool.quote_exact_in(SwapDir::BtoA, L as u64, limit).unwrap();
        assert_eq!(q.amount_out, (L / 2) as u64); // 500_000_000
        assert_eq!(q.next_sqrt_price_x64, P2);
        assert_eq!(q.fee_amount, 0);
        assert_eq!(q.amount_in_consumed, L as u64);
    }

    #[test]
    fn quote_a_to_b_fee_zero_matches_hand_value() {
        // A in = L/2 at √P=P2, limit below P1 ⇒ no cross. Output token B = delta_b floored.
        let pool = WhirlpoolPool::new(P2, L, 0);
        let limit = 1u128 << 63; // far below
        let q = pool
            .quote_exact_in(SwapDir::AtoB, (L / 2) as u64, limit)
            .unwrap();
        assert_eq!(q.amount_out, L as u64); // 1_000_000_000
        assert_eq!(q.next_sqrt_price_x64, P1);
        assert_eq!(q.fee_amount, 0);
    }

    #[test]
    fn round_trip_fee_zero_is_lossless_on_exact_fixture() {
        // B→A then A→B back, fee 0, exact power-of-two fixture ⇒ recovers the original exactly.
        let pool = WhirlpoolPool::new(P1, L, 0);
        let out = pool
            .quote_exact_in(SwapDir::BtoA, L as u64, 1u128 << 66)
            .unwrap();
        let back_pool = WhirlpoolPool::new(out.next_sqrt_price_x64, L, 0);
        let back = back_pool
            .quote_exact_in(SwapDir::AtoB, out.amount_out, 1u128 << 63)
            .unwrap();
        assert_eq!(back.amount_out, L as u64);
        assert_eq!(back.next_sqrt_price_x64, P1);
    }

    // --- fee + direction behaviour ---

    #[test]
    fn fee_reduces_output_below_the_fee_free_quote() {
        let free = WhirlpoolPool::new(P1, L, 0);
        let charged = WhirlpoolPool::new(P1, L, 3000); // 0.3%
        let limit = 1u128 << 66;
        let q_free = free.quote_exact_in(SwapDir::BtoA, L as u64, limit).unwrap();
        let q_fee = charged
            .quote_exact_in(SwapDir::BtoA, L as u64, limit)
            .unwrap();
        assert!(
            q_fee.amount_out < q_free.amount_out,
            "fee must reduce output"
        );
        assert!(q_fee.fee_amount > 0, "fee must be charged");
        // The fee path floors the input: amount_calc = floor(L · 997000 / 1e6).
        assert_eq!(q_fee.amount_in_consumed + q_fee.fee_amount, L as u64);
    }

    #[test]
    fn output_floors_and_input_ceils_per_direction() {
        // A pool/size with a fractional intermediate so rounding actually bites; assert the
        // realized output never exceeds the ideal real-valued amount (Floor on output).
        let pool = WhirlpoolPool::new(P1 + 7, L + 13, 0);
        for amt in [1u64, 7, 333, 10_001, 250_000] {
            let q = pool
                .quote_exact_in(SwapDir::BtoA, amt, 1u128 << 66)
                .unwrap();
            // next price strictly increases for b→a input.
            assert!(q.next_sqrt_price_x64 >= pool.sqrt_price_x64);
        }
    }

    // --- CrossesTick / boundary validation ---

    #[test]
    fn crosses_tick_when_limit_is_too_close() {
        let pool = WhirlpoolPool::new(P1, L, 0);
        // Limit just above current ⇒ the input reaches it ⇒ CrossesTick.
        let limit = P1 + (1u128 << 32);
        assert_eq!(
            pool.quote_exact_in(SwapDir::BtoA, L as u64, limit),
            Err(WhirlpoolError::CrossesTick)
        );
    }

    #[test]
    fn rejects_limit_on_wrong_side() {
        let pool = WhirlpoolPool::new(P2, L, 0);
        // b→a needs an UPPER limit; passing a lower one is invalid.
        assert_eq!(
            pool.quote_exact_in(SwapDir::BtoA, 10, P1),
            Err(WhirlpoolError::BadSqrtPriceLimit)
        );
        // a→b needs a LOWER limit; passing a higher one is invalid.
        assert_eq!(
            pool.quote_exact_in(SwapDir::AtoB, 10, 1u128 << 66),
            Err(WhirlpoolError::BadSqrtPriceLimit)
        );
    }

    // --- degenerate inputs ---

    #[test]
    fn zero_liquidity_and_high_fee_rejected() {
        assert_eq!(
            WhirlpoolPool::new(P1, 0, 0).quote_exact_in(SwapDir::BtoA, 1, P2),
            Err(WhirlpoolError::ZeroLiquidity)
        );
        assert_eq!(
            WhirlpoolPool::new(P1, L, 60_001).quote_exact_in(SwapDir::BtoA, 1, P2),
            Err(WhirlpoolError::FeeRateTooHigh)
        );
    }

    #[test]
    fn tiny_input_after_fee_yields_zero_output_not_error() {
        // 1 unit in at 0.3% ⇒ floor(1·997000/1e6) = 0 ⇒ zero output, price unchanged.
        let pool = WhirlpoolPool::new(P1, L, 3000);
        let q = pool.quote_exact_in(SwapDir::BtoA, 1, 1u128 << 66).unwrap();
        assert_eq!(q.amount_out, 0);
        assert_eq!(q.amount_in_consumed, 0);
        assert_eq!(q.fee_amount, 1);
        assert_eq!(q.next_sqrt_price_x64, P1);
    }

    #[test]
    fn output_is_monotonic_in_input_within_range() {
        let pool = WhirlpoolPool::new(P1, L, 3000);
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

    // helper: unwrap an AmountDelta::Valid in tests.
    fn unwrap_valid(d: AmountDelta) -> u64 {
        match d {
            AmountDelta::Valid(v) => v,
            AmountDelta::ExceedsMax => panic!("unexpected ExceedsMax"),
        }
    }
}
