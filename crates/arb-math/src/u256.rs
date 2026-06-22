//! 256-bit unsigned integer used as the wide intermediate for `mul_div` and the
//! opportunity inequality, so a `u128 * u128` product never overflows before the divide.

// The `uint` macro expansion contains its own (audited) div-ceil / assign-op patterns that
// trip these style lints; they are external-macro internals, not our code.
#[allow(clippy::manual_div_ceil, clippy::assign_op_pattern)]
mod u256_impl {
    uint::construct_uint! {
        /// 256-bit unsigned integer (4 × u64 limbs). Provides `checked_*` arithmetic plus a
        /// floor `integer_sqrt`, both used by the wide-intermediate math in this crate.
        pub struct U256(4);
    }
}
pub use u256_impl::U256;

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_small() {
        assert_eq!(U256::from(0u8).integer_sqrt(), U256::from(0u8));
        assert_eq!(U256::from(1u8).integer_sqrt(), U256::from(1u8));
        assert_eq!(U256::from(15u8).integer_sqrt(), U256::from(3u8));
        assert_eq!(U256::from(16u8).integer_sqrt(), U256::from(4u8));
        assert_eq!(U256::from(17u8).integer_sqrt(), U256::from(4u8));
        assert_eq!(U256::from(1_000_000u64).integer_sqrt(), U256::from(1000u64));
    }

    #[test]
    fn isqrt_is_floor_property() {
        for n in [2u64, 3, 99, 100, 101, 1 << 40, (1u64 << 53) + 7] {
            let r = U256::from(n).integer_sqrt();
            let r_u = r.as_u64();
            assert!(r_u.checked_mul(r_u).unwrap() <= n);
            let r1 = r_u + 1;
            assert!(r1.checked_mul(r1).map(|v| v > n).unwrap_or(true));
        }
    }
}
