//! Meteora DLMM (LB pair) bit-exact single-active-bin exact-input quoter (`sizing-12`).
//!
//! A faithful port of the `MeteoraAg/dlmm-sdk` `lb_clmm` integer math (`commons/src/extensions/
//! {bin,lb_pair}.rs`, `commons/src/math/{u64x64_math,price_math}.rs`), verified against the IDL +
//! live mainnet `LbPair HTvjzsfX3yU6BUodCjZ5vZkUrAxMDTrBs3CJaq43ashR` (fase25-venue-research,
//! 2026-06-23).
//!
//! DLMM is **constant-SUM within each bin**: a bin holds reserves `(amount_x, amount_y)` and a
//! FIXED price `P` (Q64.64) so swapping inside a bin is linear (`Y = P·X` / `X = Y/P`), with NO
//! price impact until the bin drains and the swap CROSSES to the adjacent bin. This M1 quoter
//! covers the **single active bin** (the minimal correct unit per the research guidance): if the
//! order would drain the active bin and cross, it returns [`DlmmError::CrossesBin`] rather than
//! extrapolating (multi-bin walk is Fase-3, needs the streamed `BinArray` accounts).
//!
//! ## Fee (the subtle part)
//! Total fee rate (over [`FEE_PRECISION`] = 1e9) = `min(base_fee + variable_fee, MAX_FEE_RATE)`.
//! * `base_fee = base_factor · bin_step · 10 · 10^base_fee_power_factor` — STATIC, fully
//!   quote-stable ([`base_fee_rate`]).
//! * `variable_fee = ceil( variable_fee_control · (volatility_accumulator · bin_step)^2 / 1e11 )`
//!   ([`variable_fee_rate`]) — the `volatility_accumulator` is recomputed per bin from the
//!   on-chain CLOCK at execution time, so it is **NOT** perfectly knowable at quote time. The
//!   quote is bit-exact GIVEN the resolved `total_fee_rate`; for `variable_fee_control == 0` pools
//!   the variable component is 0 and the quote is fully quote-stable. The on-chain fee is taken on
//!   the INPUT for the common `collect_fee_mode == InputOnly`, CEIL.

use crate::u256::U256;
use arb_types::SwapDir;

/// Q64.64 scale (`SCALE_OFFSET`); `ONE = 1<<64`.
const SCALE_OFFSET: u32 = 64;
const ONE: u128 = 1u128 << SCALE_OFFSET;
/// Bin-price ratio bps denominator (`BASIS_POINT_MAX`): adjacent bins differ by `1 + bin_step/1e4`.
pub const BASIS_POINT_MAX: u64 = 10_000;
/// Fee-rate denominator (`FEE_PRECISION`): rates are over 1e9 (1e9 == 100%).
pub const FEE_PRECISION: u64 = 1_000_000_000;
/// Cap on the total fee rate (`MAX_FEE_RATE` = 10%).
pub const MAX_FEE_RATE: u64 = 100_000_000;
/// Variable-fee scale-down denominator (1e11) with a `+1e11−1` ceil bias.
const VARIABLE_FEE_DIVISOR: u128 = 100_000_000_000;
/// `pow` exponent guard (`MAX_EXPONENTIAL`).
const MAX_EXPONENTIAL: u32 = 0x80000;

/// Why a DLMM quote could not be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlmmError {
    /// The (post-fee) input would drain the active bin and cross to the next — Fase-3 multi-bin.
    CrossesBin,
    /// Fee rate `>= FEE_PRECISION` (not a valid fraction).
    InvalidFee,
    /// A checked arithmetic step overflowed, or `pow` left its valid domain.
    Overflow,
}

/// The DLMM base fee rate over [`FEE_PRECISION`]:
/// `base_factor · bin_step · 10 · 10^base_fee_power_factor`.
pub fn base_fee_rate(base_factor: u16, bin_step: u16, base_fee_power_factor: u8) -> Option<u64> {
    let base = (base_factor as u64)
        .checked_mul(bin_step as u64)?
        .checked_mul(10)?;
    let pow = 10u64.checked_pow(base_fee_power_factor as u32)?;
    base.checked_mul(pow)
}

/// The DLMM variable (volatility) fee rate over [`FEE_PRECISION`]:
/// `ceil( variable_fee_control · (volatility_accumulator · bin_step)^2 / 1e11 )`. `0` when
/// `variable_fee_control == 0`. The `volatility_accumulator` is the per-bin value the on-chain
/// `update_volatility_accumulator` produces (execution-clock dependent — see module docs).
pub fn variable_fee_rate(
    volatility_accumulator: u32,
    bin_step: u16,
    variable_fee_control: u32,
) -> Option<u64> {
    if variable_fee_control == 0 {
        return Some(0);
    }
    let vfa_bin = (volatility_accumulator as u128).checked_mul(bin_step as u128)?;
    let square = vfa_bin.checked_mul(vfa_bin)?;
    let v_fee = square.checked_mul(variable_fee_control as u128)?;
    // ceil(v_fee / 1e11) = (v_fee + 1e11 - 1) / 1e11.
    let scaled = v_fee
        .checked_add(VARIABLE_FEE_DIVISOR.checked_sub(1)?)?
        .checked_div(VARIABLE_FEE_DIVISOR)?;
    u64::try_from(scaled).ok()
}

/// Total fee rate over [`FEE_PRECISION`]: `min(base + variable, MAX_FEE_RATE)`.
pub fn total_fee_rate(
    base_factor: u16,
    bin_step: u16,
    base_fee_power_factor: u8,
    volatility_accumulator: u32,
    variable_fee_control: u32,
) -> Option<u64> {
    let base = base_fee_rate(base_factor, bin_step, base_fee_power_factor)?;
    let variable = variable_fee_rate(volatility_accumulator, bin_step, variable_fee_control)?;
    Some(base.checked_add(variable)?.min(MAX_FEE_RATE))
}

/// `get_price_from_id`: the Q64.64 price of bin `active_id` for `bin_step`,
/// `(1 + bin_step/1e4)^active_id` via the `lb_clmm` `pow`. Bins normally carry this value stored;
/// this is the fallback when a `Bin.price` field is 0. `None` if `pow` leaves its domain.
pub fn get_price_from_id(active_id: i32, bin_step: u16) -> Option<u128> {
    // base = ONE + (bin_step << 64) / BASIS_POINT_MAX  (Q64.64 `1 + bin_step/1e4`).
    let bps = (bin_step as u128)
        .checked_mul(ONE)?
        .checked_div(BASIS_POINT_MAX as u128)?;
    let base = ONE.checked_add(bps)?;
    pow_q64(base, active_id)
}

/// `lb_clmm::u64x64_math::pow`: `base^exp` in Q64.64 via exponentiation-by-squaring with the
/// `u128::MAX / x` inversion trick that keeps every intermediate `< 2^128`.
fn pow_q64(base: u128, exp: i32) -> Option<u128> {
    if exp == 0 {
        return Some(ONE);
    }
    let mut invert = exp < 0;
    let exp_abs = exp.unsigned_abs();
    if exp_abs >= MAX_EXPONENTIAL {
        return None;
    }
    let mut squared_base = base;
    let mut result = ONE;
    // Keep the running base below ONE so products never exceed 2^128.
    if squared_base >= result {
        squared_base = u128::MAX.checked_div(squared_base)?;
        invert = !invert;
    }
    // Unrolled bits 0x1 .. 0x40000 (19 squarings); `>> 64` == `/ ONE`, `·` is checked.
    let mut bit = 1u32;
    while bit < MAX_EXPONENTIAL {
        if exp_abs & bit != 0 {
            result = result.checked_mul(squared_base)?.checked_div(ONE)?;
        }
        // Square the base for the next bit (skip the final redundant squaring).
        if bit < MAX_EXPONENTIAL >> 1 {
            squared_base = squared_base.checked_mul(squared_base)?.checked_div(ONE)?;
        }
        bit <<= 1;
    }
    if result == 0 {
        return None;
    }
    if invert {
        result = u128::MAX.checked_div(result)?;
    }
    Some(result)
}

/// A DLMM active bin's swap-relevant state: its fixed Q64.64 `price` and its `(amount_x, amount_y)`
/// reserves (the maximum output available in this bin per direction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlmmActiveBin {
    pub price_x64: u128,
    pub amount_x: u64,
    pub amount_y: u64,
}

/// The result of a single-active-bin exact-input quote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlmmQuote {
    /// Tokens out of this bin (before any Token-2022 transfer fee).
    pub amount_out: u64,
    /// Trading fee taken (input-side for `InputOnly`, output-side otherwise).
    pub fee_amount: u64,
}

impl DlmmActiveBin {
    pub fn new(price_x64: u128, amount_x: u64, amount_y: u64) -> Self {
        Self {
            price_x64,
            amount_x,
            amount_y,
        }
    }

    /// Bit-exact single-bin exact-input quote. `total_fee_rate` is over [`FEE_PRECISION`]
    /// (resolve via [`total_fee_rate`]); `fee_on_input` is `collect_fee_mode == InputOnly`
    /// (or `OnlyY && !swap_for_y`). `SwapDir::AtoB` is `swap_for_y` (token X in, token Y out).
    /// Returns [`DlmmError::CrossesBin`] if the post-fee input would drain this bin.
    pub fn quote_exact_in(
        &self,
        dir: SwapDir,
        amount_in: u64,
        total_fee_rate: u64,
        fee_on_input: bool,
    ) -> Result<DlmmQuote, DlmmError> {
        if total_fee_rate >= FEE_PRECISION {
            return Err(DlmmError::InvalidFee);
        }
        let swap_for_y = matches!(dir, SwapDir::AtoB);
        let price = self.price_x64;

        // Fee on input, CEIL (compute_fee_from_amount): only the remainder enters the bin.
        let (net_in, fee_in) = if fee_on_input {
            let fee = compute_fee_from_amount(amount_in, total_fee_rate)?;
            (amount_in.checked_sub(fee).ok_or(DlmmError::Overflow)?, fee)
        } else {
            (amount_in, 0)
        };

        // Capacity to fully drain this bin (get_amount_in, Rounding::Up). If the net input reaches
        // it, the swap crosses to the next bin — out of this M1 single-bin quoter's scope.
        let max_out = if swap_for_y {
            self.amount_y
        } else {
            self.amount_x
        };
        let max_in_full = get_amount_in(max_out, price, swap_for_y)?;
        if net_in >= max_in_full {
            return Err(DlmmError::CrossesBin);
        }

        // Constant-sum output within the bin (get_amount_out, Rounding::Down).
        let gross_out = get_amount_out(net_in, price, swap_for_y)?;

        // Fee on output, CEIL, when not InputOnly.
        let (amount_out, fee_out) = if fee_on_input {
            (gross_out, 0)
        } else {
            let fee = compute_fee_from_amount(gross_out, total_fee_rate)?;
            (gross_out.checked_sub(fee).ok_or(DlmmError::Overflow)?, fee)
        };

        Ok(DlmmQuote {
            amount_out,
            fee_amount: fee_in.checked_add(fee_out).ok_or(DlmmError::Overflow)?,
        })
    }
}

/// `compute_fee_from_amount`: `ceil(amount · rate / FEE_PRECISION)` — fee carved out of a gross.
fn compute_fee_from_amount(amount: u64, rate: u64) -> Result<u64, DlmmError> {
    if amount == 0 || rate == 0 {
        return Ok(0);
    }
    let num = (amount as u128)
        .checked_mul(rate as u128)
        .ok_or(DlmmError::Overflow)?;
    let fee = num
        .checked_add(
            (FEE_PRECISION as u128)
                .checked_sub(1)
                .ok_or(DlmmError::Overflow)?,
        )
        .ok_or(DlmmError::Overflow)?
        .checked_div(FEE_PRECISION as u128)
        .ok_or(DlmmError::Overflow)?;
    u64::try_from(fee).map_err(|_| DlmmError::Overflow)
}

/// `Bin::get_amount_out` (Rounding::Down): `swap_for_y ⇒ floor(price·in >> 64)`,
/// else `floor((in << 64) / price)`.
fn get_amount_out(net_in: u64, price: u128, swap_for_y: bool) -> Result<u64, DlmmError> {
    let out = if swap_for_y {
        mul_shr_64(price, net_in as u128, false)?
    } else {
        if price == 0 {
            return Err(DlmmError::Overflow);
        }
        shl64_div(net_in as u128, price, false)?
    };
    u64::try_from(out).map_err(|_| DlmmError::Overflow)
}

/// `Bin::get_amount_in` (Rounding::Up) — input needed to take `out` from the bin:
/// `swap_for_y ⇒ ceil((out << 64) / price)`, else `ceil(out·price >> 64)`.
fn get_amount_in(out: u64, price: u128, swap_for_y: bool) -> Result<u64, DlmmError> {
    let amt = if swap_for_y {
        if price == 0 {
            return Err(DlmmError::Overflow);
        }
        shl64_div(out as u128, price, true)?
    } else {
        mul_shr_64(out as u128, price, true)?
    };
    u64::try_from(amt).map_err(|_| DlmmError::Overflow)
}

/// `(a · b) >> 64` with the requested rounding, via a 256-bit intermediate.
fn mul_shr_64(a: u128, b: u128, round_up: bool) -> Result<u128, DlmmError> {
    let prod = U256::from(a)
        .checked_mul(U256::from(b))
        .ok_or(DlmmError::Overflow)?;
    let res = if round_up {
        div_ceil_256(prod, U256::from(ONE))?
    } else {
        prod.checked_div(U256::from(ONE))
            .ok_or(DlmmError::Overflow)?
    };
    u256_to_u128(res)
}

/// `(a << 64) / p` with the requested rounding, via a 256-bit intermediate (`p` non-zero).
fn shl64_div(a: u128, p: u128, round_up: bool) -> Result<u128, DlmmError> {
    let num = U256::from(a)
        .checked_mul(U256::from(ONE))
        .ok_or(DlmmError::Overflow)?;
    let res = if round_up {
        div_ceil_256(num, U256::from(p))?
    } else {
        num.checked_div(U256::from(p)).ok_or(DlmmError::Overflow)?
    };
    u256_to_u128(res)
}

fn div_ceil_256(num: U256, d: U256) -> Result<U256, DlmmError> {
    let q = num.checked_div(d).ok_or(DlmmError::Overflow)?;
    let prod = q.checked_mul(d).ok_or(DlmmError::Overflow)?;
    if prod < num {
        q.checked_add(U256::one()).ok_or(DlmmError::Overflow)
    } else {
        Ok(q)
    }
}

#[inline]
fn u256_to_u128(v: U256) -> Result<u128, DlmmError> {
    if v > U256::from(u128::MAX) {
        Err(DlmmError::Overflow)
    } else {
        Ok(v.as_u128())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn base_fee_matches_formula() {
        // SOL/USDC live: base_factor=10000, bin_step=1, power=0 ⇒ 10000·1·10 = 100_000 over 1e9.
        assert_eq!(base_fee_rate(10_000, 1, 0), Some(100_000));
        // power factor scales by 10^power.
        assert_eq!(base_fee_rate(10_000, 1, 2), Some(10_000_000));
        assert_eq!(FEE_PRECISION, 1_000_000_000);
    }

    #[test]
    fn variable_fee_is_zero_when_control_zero_and_ceils_otherwise() {
        assert_eq!(variable_fee_rate(50_000, 1, 0), Some(0));
        // ceil((va·bin_step)^2 · vfc / 1e11): va=1000, step=1, vfc=2_000_000
        // (1000)^2 = 1e6; ·2e6 = 2e12; ceil(2e12/1e11) = 20.
        assert_eq!(variable_fee_rate(1_000, 1, 2_000_000), Some(20));
        // base alone = 10000·10000·10 = 1e9 > MAX_FEE_RATE ⇒ total clamps to the cap.
        let capped = total_fee_rate(10_000, 10_000, 0, 0, 0);
        assert_eq!(capped, Some(MAX_FEE_RATE));
    }

    #[test]
    fn price_from_id_zero_is_one_and_is_monotonic() {
        assert_eq!(get_price_from_id(0, 25), Some(ONE));
        // bin_step=10000 ⇒ base=2.0 ⇒ id=1 ≈ 2.0 (within a few ulps of the inversion trick).
        let p1 = get_price_from_id(1, 10_000).unwrap();
        let two = 2u128 * ONE;
        let diff = p1.abs_diff(two);
        assert!(diff < 16, "id=1 price {p1} should be ≈ 2·ONE, diff {diff}");
        // Strictly increasing in id around 0.
        let pm1 = get_price_from_id(-1, 25).unwrap();
        let p0 = get_price_from_id(0, 25).unwrap();
        let pp1 = get_price_from_id(1, 25).unwrap();
        assert!(pm1 < p0 && p0 < pp1, "{pm1} < {p0} < {pp1}");
    }

    #[test]
    fn constant_sum_output_at_price_one_no_fee() {
        // price 1.0, big Y reserve, fee 0: X in = Y out (linear, no impact) while it fits the bin.
        let bin = DlmmActiveBin::new(ONE, 1_000_000, 1_000_000);
        let q = bin.quote_exact_in(SwapDir::AtoB, 10_000, 0, true).unwrap();
        assert_eq!(q.amount_out, 10_000); // Y = 1.0 · X
        assert_eq!(q.fee_amount, 0);
        // Y->X direction is also 1:1 at price 1.0.
        let q2 = bin.quote_exact_in(SwapDir::BtoA, 10_000, 0, true).unwrap();
        assert_eq!(q2.amount_out, 10_000);
    }

    #[test]
    fn constant_sum_output_at_price_two() {
        // price 2.0 (Y per X): X in ⇒ 2·X out of Y; Y in ⇒ Y/2 out of X.
        let bin = DlmmActiveBin::new(2 * ONE, 1_000_000, 1_000_000);
        let xy = bin.quote_exact_in(SwapDir::AtoB, 1_000, 0, true).unwrap();
        assert_eq!(xy.amount_out, 2_000);
        let yx = bin.quote_exact_in(SwapDir::BtoA, 1_000, 0, true).unwrap();
        assert_eq!(yx.amount_out, 500);
    }

    #[test]
    fn fee_on_input_ceils_and_reduces_output() {
        // price 1.0, 0.1% fee on input: net_in = in - ceil(in·1e6/1e9).
        let bin = DlmmActiveBin::new(ONE, 1_000_000, 1_000_000);
        let rate = 1_000_000; // 0.1% over 1e9
        let q = bin
            .quote_exact_in(SwapDir::AtoB, 100_000, rate, true)
            .unwrap();
        let fee = (100_000u128 * rate as u128).div_ceil(FEE_PRECISION as u128) as u64;
        assert_eq!(q.fee_amount, fee);
        assert_eq!(q.amount_out, 100_000 - fee); // price 1.0 ⇒ out == net_in
    }

    #[test]
    fn crosses_bin_when_order_would_drain_active_bin() {
        // Bin holds only 5_000 Y; a 10_000 X order at price 1.0 would drain it ⇒ CrossesBin.
        let bin = DlmmActiveBin::new(ONE, 1_000_000, 5_000);
        assert_eq!(
            bin.quote_exact_in(SwapDir::AtoB, 10_000, 0, true),
            Err(DlmmError::CrossesBin)
        );
    }

    #[test]
    fn invalid_fee_rejected() {
        let bin = DlmmActiveBin::new(ONE, 1_000_000, 1_000_000);
        assert_eq!(
            bin.quote_exact_in(SwapDir::AtoB, 1, FEE_PRECISION, true),
            Err(DlmmError::InvalidFee)
        );
    }
}
