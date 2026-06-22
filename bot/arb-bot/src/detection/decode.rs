//! Per-venue account decoding. The verifiable primitives — SPL vault `amount` read,
//! sqrtPriceX64→price, Anchor discriminator check — are implemented and tested here. The
//! exact field OFFSETS inside each venue's Anchor account (PoolState / Whirlpool / PumpSwap)
//! are documented placeholders to be confirmed against each IDL (same status as the on-chain
//! adapter discriminators) and validated by the Surfpool integration once `build-sbf` exists.

use arb_math::CpmmReserves;

/// SPL/Token-2022 token-account `amount` (u64 LE) at offset 64 — the vault reserve.
pub fn read_vault_amount(data: &[u8]) -> Option<u64> {
    let bytes = data.get(64..72)?;
    let arr: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(arr))
}

/// Build CPMM reserves from the two vault balances + the pool's fee (Raydium CPMM, PumpSwap).
pub fn cpmm_reserves_from_vaults(
    vault_a_data: &[u8],
    vault_b_data: &[u8],
    fee_numerator: u64,
    fee_denominator: u64,
) -> Option<CpmmReserves> {
    let a = read_vault_amount(vault_a_data)?;
    let b = read_vault_amount(vault_b_data)?;
    Some(CpmmReserves::new(a, b, fee_numerator, fee_denominator))
}

/// Convert an Orca/Raydium-CLMM `sqrtPriceX64` (Q64.64) into a float price (token-B per
/// token-A): `price = (sqrtPrice / 2^64)^2`.
pub fn sqrt_price_x64_to_price(sqrt_price_x64: u128) -> f64 {
    let q = sqrt_price_x64 as f64 / (2f64).powi(64);
    q * q
}

/// First 8 bytes of an Anchor account are its type discriminator; verify before casting from
/// offset 8. Returns false if the buffer is too short or the discriminator mismatches.
pub fn has_anchor_discriminator(data: &[u8], expected: &[u8; 8]) -> bool {
    data.get(0..8).map(|d| d == expected).unwrap_or(false)
}

/// Whirlpool field offsets (after the 8-byte discriminator). TODO(M1): confirm against the
/// whirlpool IDL; currently documentary so the decoder shape is fixed.
pub mod whirlpool_offsets {
    /// `sqrt_price: u128` — placeholder offset; verify against IDL.
    pub const SQRT_PRICE: usize = 65;
    /// `liquidity: u128` — placeholder offset; verify against IDL.
    pub const LIQUIDITY: usize = 49;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_amount_reads_offset_64() {
        let mut buf = [0u8; 72];
        buf[64..72].copy_from_slice(&987_654u64.to_le_bytes());
        assert_eq!(read_vault_amount(&buf), Some(987_654));
        assert_eq!(read_vault_amount(&[0u8; 10]), None);
    }

    #[test]
    fn reserves_from_two_vaults() {
        let mut a = [0u8; 72];
        let mut b = [0u8; 72];
        a[64..72].copy_from_slice(&1_000u64.to_le_bytes());
        b[64..72].copy_from_slice(&2_000u64.to_le_bytes());
        let r = cpmm_reserves_from_vaults(&a, &b, 25, 10_000).unwrap();
        assert_eq!((r.reserve_a, r.reserve_b), (1_000, 2_000));
    }

    #[test]
    fn sqrt_price_conversion() {
        // sqrtPrice = 2^64 -> q = 1.0 -> price = 1.0
        assert!((sqrt_price_x64_to_price(1u128 << 64) - 1.0).abs() < 1e-9);
        // sqrtPrice = 2^65 -> q = 2.0 -> price = 4.0
        assert!((sqrt_price_x64_to_price(1u128 << 65) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn discriminator_check() {
        let disc = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut data = vec![0u8; 16];
        data[0..8].copy_from_slice(&disc);
        assert!(has_anchor_discriminator(&data, &disc));
        assert!(!has_anchor_discriminator(&data, &[9; 8]));
        assert!(!has_anchor_discriminator(&[0u8; 4], &disc));
    }
}
