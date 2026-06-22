//! `mul_div` with a 256-bit intermediate: computes `floor(a*b/denom)` / `ceil(a*b/denom)`
//! without overflowing the `a*b` product. Returns `None` on `denom == 0` or if the result
//! does not fit back into `u128`.

use crate::u256::U256;

/// `floor(a * b / denom)`.
#[inline]
pub fn mul_div_floor(a: u128, b: u128, denom: u128) -> Option<u128> {
    if denom == 0 {
        return None;
    }
    let prod = U256::from(a).checked_mul(U256::from(b))?;
    let q = prod.checked_div(U256::from(denom))?;
    if q > U256::from(u128::MAX) {
        None
    } else {
        Some(q.as_u128())
    }
}

/// `ceil(a * b / denom)`.
#[inline]
pub fn mul_div_ceil(a: u128, b: u128, denom: u128) -> Option<u128> {
    if denom == 0 {
        return None;
    }
    let prod = U256::from(a).checked_mul(U256::from(b))?;
    // ceil(p/d) = (p + d - 1) / d  (no overflow: done in U256)
    let d = U256::from(denom);
    let adj = prod.checked_add(d)?.checked_sub(U256::one())?;
    let q = adj.checked_div(d)?;
    if q > U256::from(u128::MAX) {
        None
    } else {
        Some(q.as_u128())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn floor_and_ceil_basic() {
        assert_eq!(mul_div_floor(7, 3, 2), Some(10)); // 21/2 = 10.5 -> 10
        assert_eq!(mul_div_ceil(7, 3, 2), Some(11)); //              -> 11
        assert_eq!(mul_div_floor(10, 2, 5), Some(4)); // exact
        assert_eq!(mul_div_ceil(10, 2, 5), Some(4)); // exact: no +1
    }

    #[test]
    fn no_overflow_on_max_inputs() {
        // u128::MAX * u128::MAX / u128::MAX == u128::MAX, must not overflow.
        assert_eq!(
            mul_div_floor(u128::MAX, u128::MAX, u128::MAX),
            Some(u128::MAX)
        );
    }

    #[test]
    fn div_by_zero_is_none() {
        assert_eq!(mul_div_floor(1, 1, 0), None);
        assert_eq!(mul_div_ceil(1, 1, 0), None);
    }

    #[test]
    fn result_overflow_is_none() {
        // (u128::MAX * 2) / 1 does not fit in u128.
        assert_eq!(mul_div_floor(u128::MAX, 2, 1), None);
    }
}
