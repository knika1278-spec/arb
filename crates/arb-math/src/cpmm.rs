//! Constant-product (`x*y=k`) swap math, bit-exact to the Raydium CP-Swap integer path:
//! fee taken on the **input**, every division **floored** for output (Ceil for required
//! input). This is the M1-GATE target: off-chain `quote_out` MUST equal the on-chain CPI's
//! realized output for the same `(reserves, fee, amount_in)`.
//!
//! NOTE on Orca Whirlpool: within a single tick range it is also constant-product, but the
//! authoritative realized amount comes from sqrtPriceX64 tick math and can cross ticks.
//! `crate::venue` wraps this for Raydium/PumpSwap; the exact Whirlpool sqrt-price mirror is
//! a Fase-1 task validated against the on-chain CPI differential (see implementation-plan
//! §5.3) — do not assume this CP formula is bit-exact for Orca across tick crossings.

use crate::mul_div::{mul_div_ceil, mul_div_floor};
use arb_types::SwapDir;

/// A constant-product pool's two reserves plus its fee, expressed as a rational
/// `fee_numerator / fee_denominator` (e.g. Raydium 25bps = 25/10_000; Orca fee_rate `r`
/// out of 1_000_000 = r/1_000_000).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpmmReserves {
    /// Reserve of token A (the base/`AtoB`-input side).
    pub reserve_a: u64,
    /// Reserve of token B.
    pub reserve_b: u64,
    pub fee_numerator: u64,
    pub fee_denominator: u64,
}

impl CpmmReserves {
    pub fn new(reserve_a: u64, reserve_b: u64, fee_numerator: u64, fee_denominator: u64) -> Self {
        Self {
            reserve_a,
            reserve_b,
            fee_numerator,
            fee_denominator,
        }
    }

    /// `(reserve_in, reserve_out)` for a given direction.
    #[inline]
    fn oriented(&self, dir: SwapDir) -> (u64, u64) {
        match dir {
            SwapDir::AtoB => (self.reserve_a, self.reserve_b),
            SwapDir::BtoA => (self.reserve_b, self.reserve_a),
        }
    }

    /// Floored swap output for `amount_in` in `dir`. Mirrors Raydium `swap_base_in`.
    pub fn quote_out(&self, dir: SwapDir, amount_in: u64) -> Option<u64> {
        let (reserve_in, reserve_out) = self.oriented(dir);
        quote_out(
            reserve_in,
            reserve_out,
            self.fee_numerator,
            self.fee_denominator,
            amount_in,
        )
    }

    /// Ceiled required input to obtain `amount_out` in `dir`. Mirrors `swap_base_out`.
    pub fn required_in(&self, dir: SwapDir, amount_out: u64) -> Option<u64> {
        let (reserve_in, reserve_out) = self.oriented(dir);
        required_in(
            reserve_in,
            reserve_out,
            self.fee_numerator,
            self.fee_denominator,
            amount_out,
        )
    }
}

/// Floored constant-product output. `fee` is taken on the input first (floor), then the
/// invariant gives `out = floor(reserve_out * in_after_fee / (reserve_in + in_after_fee))`.
pub fn quote_out(
    reserve_in: u64,
    reserve_out: u64,
    fee_numerator: u64,
    fee_denominator: u64,
    amount_in: u64,
) -> Option<u64> {
    if fee_denominator == 0 || fee_numerator > fee_denominator {
        return None;
    }
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return Some(0);
    }
    let net = (fee_denominator as u128).checked_sub(fee_numerator as u128)?;
    let in_after_fee = mul_div_floor(amount_in as u128, net, fee_denominator as u128)?;
    if in_after_fee == 0 {
        return Some(0);
    }
    let denom = (reserve_in as u128).checked_add(in_after_fee)?;
    let out = mul_div_floor(reserve_out as u128, in_after_fee, denom)?;
    // Never let a rounding artifact claim the whole reserve.
    let out = out.min((reserve_out as u128).saturating_sub(1));
    u64::try_from(out).ok()
}

/// Ceiled required gross input to receive exactly `amount_out` (favoring the pool).
pub fn required_in(
    reserve_in: u64,
    reserve_out: u64,
    fee_numerator: u64,
    fee_denominator: u64,
    amount_out: u64,
) -> Option<u64> {
    if fee_denominator == 0 || fee_numerator >= fee_denominator {
        return None;
    }
    if amount_out == 0 {
        return Some(0);
    }
    if amount_out as u128 >= reserve_out as u128 {
        return None; // cannot drain the pool
    }
    // in_after_fee = ceil(reserve_in * amount_out / (reserve_out - amount_out))
    let denom = (reserve_out as u128).checked_sub(amount_out as u128)?;
    let in_after_fee = mul_div_ceil(reserve_in as u128, amount_out as u128, denom)?;
    // gross = ceil(in_after_fee * fee_denominator / (fee_denominator - fee_numerator))
    let net = (fee_denominator as u128).checked_sub(fee_numerator as u128)?;
    let gross = mul_div_ceil(in_after_fee, fee_denominator as u128, net)?;
    u64::try_from(gross).ok()
}

/// A two-pool round-trip: input base `X` -> pool A (`dir_a`) -> token `Y`
/// -> pool B (`dir_b`) -> back to `X`. The carry between legs is the realized output of
/// leg A fed as the input of leg B (balance-delta chaining, invariant §7).
#[derive(Clone, Copy, Debug)]
pub struct RoundTrip {
    pub pool_a: CpmmReserves,
    pub dir_a: SwapDir,
    pub pool_b: CpmmReserves,
    pub dir_b: SwapDir,
}

impl RoundTrip {
    pub fn new(pool_a: CpmmReserves, dir_a: SwapDir, pool_b: CpmmReserves, dir_b: SwapDir) -> Self {
        Self {
            pool_a,
            dir_a,
            pool_b,
            dir_b,
        }
    }

    /// Final amount of `X` out after both legs for an input `delta_in` of `X`.
    /// Exact integer chaining — this is what the on-chain assert effectively recomputes.
    pub fn realized_out(&self, delta_in: u64) -> Option<u64> {
        let mid = self.pool_a.quote_out(self.dir_a, delta_in)?;
        self.pool_b.quote_out(self.dir_b, mid)
    }

    /// Net profit (final_out - delta_in) as a signed integer; `None` on arithmetic failure.
    pub fn profit(&self, delta_in: u64) -> Option<i128> {
        let out = self.realized_out(delta_in)?;
        (out as i128).checked_sub(delta_in as i128)
    }
}

/// Exact opportunity test for the round-trip composite (plan.md §7):
/// arbitrage exists iff `g_a·g_b·Ra_out·Rb_out > Ra_in·Rb_in`, computed with integer
/// fee factors in a 256-bit intermediate (no float, no overflow):
/// `(da-fa)(db-fb)·Ra_out·Rb_out  >  da·db·Ra_in·Rb_in`.
pub fn opportunity_exists(rt: &RoundTrip) -> bool {
    use crate::u256::U256;
    let (ra_in, ra_out) = match rt.dir_a {
        SwapDir::AtoB => (rt.pool_a.reserve_a, rt.pool_a.reserve_b),
        SwapDir::BtoA => (rt.pool_a.reserve_b, rt.pool_a.reserve_a),
    };
    let (rb_in, rb_out) = match rt.dir_b {
        SwapDir::AtoB => (rt.pool_b.reserve_a, rt.pool_b.reserve_b),
        SwapDir::BtoA => (rt.pool_b.reserve_b, rt.pool_b.reserve_a),
    };
    let (da, fa) = (rt.pool_a.fee_denominator, rt.pool_a.fee_numerator);
    let (db, fb) = (rt.pool_b.fee_denominator, rt.pool_b.fee_numerator);
    if da == 0 || db == 0 || fa > da || fb > db {
        return false;
    }
    let ga = match da.checked_sub(fa) {
        Some(v) => v,
        None => return false,
    };
    let gb = match db.checked_sub(fb) {
        Some(v) => v,
        None => return false,
    };
    // lhs = ga*gb*ra_out*rb_out ; rhs = da*db*ra_in*rb_in
    let mul4 = |a: u64, b: u64, c: u64, d: u64| -> Option<U256> {
        U256::from(a)
            .checked_mul(U256::from(b))?
            .checked_mul(U256::from(c))?
            .checked_mul(U256::from(d))
    };
    match (mul4(ga, gb, ra_out, rb_out), mul4(da, db, ra_in, rb_in)) {
        (Some(lhs), Some(rhs)) => lhs > rhs,
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    // Raydium-style 25 bps fee.
    fn pool(a: u64, b: u64) -> CpmmReserves {
        CpmmReserves::new(a, b, 25, 10_000)
    }

    #[test]
    fn quote_out_matches_hand_computation() {
        // reserves 1_000_000 / 1_000_000, fee 25bps, amount_in = 10_000
        // in_after_fee = floor(10000 * 9975 / 10000) = 9975
        // out = floor(1_000_000 * 9975 / (1_000_000 + 9975)) = floor(9975000000/1009975) = 9876
        let p = pool(1_000_000, 1_000_000);
        assert_eq!(p.quote_out(SwapDir::AtoB, 10_000), Some(9876));
    }

    #[test]
    fn required_in_inverts_quote_out_within_rounding() {
        let p = pool(5_000_000, 3_000_000);
        for amt_in in [1u64, 10, 1000, 50_000, 250_000] {
            let out = p.quote_out(SwapDir::AtoB, amt_in).unwrap();
            if out == 0 {
                continue;
            }
            let need = p.required_in(SwapDir::AtoB, out).unwrap();
            // required_in ceils & favors the pool: needed input <= original (never more),
            // and re-quoting `need` yields at least `out`.
            assert!(need <= amt_in, "need={need} amt_in={amt_in}");
            assert!(p.quote_out(SwapDir::AtoB, need).unwrap() >= out);
        }
    }

    #[test]
    fn edge_cases() {
        let p = pool(1_000, 1_000);
        assert_eq!(p.quote_out(SwapDir::AtoB, 0), Some(0));
        assert_eq!(
            CpmmReserves::new(0, 1000, 25, 10000).quote_out(SwapDir::AtoB, 100),
            Some(0)
        );
        assert_eq!(p.required_in(SwapDir::AtoB, 1000), None); // can't drain
    }

    #[test]
    fn roundtrip_profit_sign() {
        // Pool A cheap (lots of Y per X), pool B sells Y back for more X — round-trip edge.
        // A: 1M X / 2M Y ; B: 2M Y / 1.1M X. Spot edge ~10%, but the pools are only 1M, so
        // the optimum size is small — a 5% trade (50k) is past the peak and LOSES (correct).
        let a = CpmmReserves::new(1_000_000, 2_000_000, 25, 10_000);
        let b = CpmmReserves::new(2_000_000, 1_100_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB);
        assert!(opportunity_exists(&rt));
        // A small size is profitable; the oversized 50k one is correctly unprofitable.
        assert!(rt.profit(5_000).unwrap() > 0, "small size should profit");
        assert!(
            rt.profit(50_000).unwrap() < 0,
            "oversized trade past the peak should lose"
        );
    }

    #[test]
    fn no_opportunity_when_balanced() {
        let a = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let b = CpmmReserves::new(1_000_000, 1_000_000, 25, 10_000);
        let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::BtoA);
        assert!(!opportunity_exists(&rt));
        // Round-trip through equal pools must lose to fees.
        assert!(rt.profit(10_000).unwrap() < 0);
    }
}
