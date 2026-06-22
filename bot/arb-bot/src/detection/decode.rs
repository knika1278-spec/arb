//! Per-venue account decoding. The Orca **Whirlpool** decoder + field offsets are **VERIFIED**
//! (2026-06-23) against the canonical `Whirlpool` struct and three real mainnet accounts — see
//! [`decode_whirlpool`] / [`whirlpool_offsets`] and the `decodes_real_whirlpool_account` test.
//! Raydium CPMM `PoolState` + PumpSwap `Pool` field offsets are still TODO-verify against their
//! IDLs/real accounts. The verifiable primitives — SPL vault `amount` read, sqrtPriceX64→price,
//! Anchor discriminator check — are implemented and tested here.

use arb_math::CpmmReserves;
use solana_pubkey::Pubkey;

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

/// Little-endian fixed-width readers over an account byte slice. All bounds-checked (return
/// `None` past the end) so a short/garbage account can never panic the decode path.
#[inline]
fn read_u16(d: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        d.get(off..off.checked_add(2)?)?.try_into().ok()?,
    ))
}
#[inline]
fn read_i32(d: &[u8], off: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        d.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}
#[inline]
fn read_u128(d: &[u8], off: usize) -> Option<u128> {
    Some(u128::from_le_bytes(
        d.get(off..off.checked_add(16)?)?.try_into().ok()?,
    ))
}
#[inline]
fn read_pubkey(d: &[u8], off: usize) -> Option<Pubkey> {
    let bytes: [u8; 32] = d.get(off..off.checked_add(32)?)?.try_into().ok()?;
    Some(Pubkey::new_from_array(bytes))
}

/// Orca Whirlpool Anchor account discriminator = `sha256("account:Whirlpool")[..8]`.
/// VERIFIED 2026-06-23: equals `data[0..8]` of real mainnet whirlpool accounts.
pub const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];

/// Orca Whirlpool field offsets (bytes from the start of the account, i.e. INCLUDING the
/// 8-byte Anchor discriminator). **VERIFIED 2026-06-23** against the canonical `Whirlpool`
/// struct (orca-so/whirlpools `state/whirlpool.rs`) AND three real mainnet SOL/USDC
/// whirlpools via `getAccountInfo`: `tick_current_index` ↔ `sqrt_price` are mutually
/// consistent and `token_mint_{a,b}` decode to SOL/USDC, pinning every offset below.
pub mod whirlpool_offsets {
    /// `tick_spacing: u16`.
    pub const TICK_SPACING: usize = 41;
    /// `fee_rate: u16` (out of 1_000_000).
    pub const FEE_RATE: usize = 45;
    /// `liquidity: u128`.
    pub const LIQUIDITY: usize = 49;
    /// `sqrt_price: u128` (Q64.64).
    pub const SQRT_PRICE: usize = 65;
    /// `tick_current_index: i32`.
    pub const TICK_CURRENT_INDEX: usize = 81;
    /// `token_mint_a: Pubkey`.
    pub const TOKEN_MINT_A: usize = 101;
    /// `token_vault_a: Pubkey`.
    pub const TOKEN_VAULT_A: usize = 133;
    /// `token_mint_b: Pubkey`.
    pub const TOKEN_MINT_B: usize = 181;
    /// `token_vault_b: Pubkey`.
    pub const TOKEN_VAULT_B: usize = 213;
}

/// Decoded swap-relevant subset of an Orca Whirlpool account. `sqrt_price_x64`, `liquidity`,
/// and `fee_rate` feed [`arb_math::WhirlpoolPool`] (the bit-exact quoter, sizing-5);
/// `tick_current_index`/`tick_spacing` feed the add-6 tick-array resolver; the mints/vaults
/// drive routing + the CPI account list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WhirlpoolState {
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current_index: i32,
    pub tick_spacing: u16,
    pub fee_rate: u16,
    pub mint_a: Pubkey,
    pub vault_a: Pubkey,
    pub mint_b: Pubkey,
    pub vault_b: Pubkey,
}

impl WhirlpoolState {
    /// The bit-exact quoter view ([`arb_math::WhirlpoolPool::quote_exact_in`]).
    pub fn quoter(&self) -> arb_math::WhirlpoolPool {
        arb_math::WhirlpoolPool::new(self.sqrt_price_x64, self.liquidity, self.fee_rate)
    }
}

/// Decode an Orca Whirlpool account. Returns `None` on a wrong/short discriminator or a
/// truncated account (never panics).
pub fn decode_whirlpool(data: &[u8]) -> Option<WhirlpoolState> {
    if !has_anchor_discriminator(data, &WHIRLPOOL_DISCRIMINATOR) {
        return None;
    }
    use whirlpool_offsets as o;
    Some(WhirlpoolState {
        liquidity: read_u128(data, o::LIQUIDITY)?,
        sqrt_price_x64: read_u128(data, o::SQRT_PRICE)?,
        tick_current_index: read_i32(data, o::TICK_CURRENT_INDEX)?,
        tick_spacing: read_u16(data, o::TICK_SPACING)?,
        fee_rate: read_u16(data, o::FEE_RATE)?,
        mint_a: read_pubkey(data, o::TOKEN_MINT_A)?,
        vault_a: read_pubkey(data, o::TOKEN_VAULT_A)?,
        mint_b: read_pubkey(data, o::TOKEN_MINT_B)?,
        vault_b: read_pubkey(data, o::TOKEN_VAULT_B)?,
    })
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

    #[test]
    fn decodes_real_whirlpool_account() {
        // Real mainnet SOL/USDC whirlpool HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ —
        // a frozen `getAccountInfo` snapshot (2026-06-23). This is the detection-3 "tested
        // against real cloned account bytes, not synthetic" gate: it proves every offset
        // reads the right field.
        let data = include_bytes!("fixtures/whirlpool_sol_usdc_hjpjow.bin");
        assert_eq!(&data[0..8], &WHIRLPOOL_DISCRIMINATOR);
        let w = decode_whirlpool(data).expect("real whirlpool decodes");

        // Stable structural fields — a pool's mints/vaults/tick_spacing/fee_rate never change.
        assert_eq!(
            w.mint_a,
            "So11111111111111111111111111111111111111112"
                .parse()
                .unwrap()
        );
        assert_eq!(
            w.mint_b,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
                .parse()
                .unwrap()
        );
        assert_eq!(
            w.vault_a,
            "3YQm7ujtXWJU2e9jhp2QGHpnn1ShXn12QjvzMvDgabpX"
                .parse()
                .unwrap()
        );
        assert_eq!(
            w.vault_b,
            "2JTw1fE2wz1SymWUQ7UqpVtrTuKjcd6mWwYwUJUCh2rq"
                .parse()
                .unwrap()
        );
        assert_eq!(w.tick_spacing, 64);
        assert_eq!(w.fee_rate, 3000);

        // Volatile fields, frozen in this snapshot — exact values prove the u128/i32 offsets.
        assert_eq!(w.liquidity, 205_233_605_918);
        assert_eq!(w.sqrt_price_x64, 4_974_723_863_729_489_614);
        assert_eq!(w.tick_current_index, -26_212);

        // The quoter view ([`arb_math::WhirlpoolPool`]) carries the swap-relevant fields.
        let q = w.quoter();
        assert_eq!(q.sqrt_price_x64, w.sqrt_price_x64);
        assert_eq!(q.liquidity, w.liquidity);
        assert_eq!(q.fee_rate, w.fee_rate);

        // Wrong/zero discriminator => None, never a panic.
        assert!(decode_whirlpool(&[0u8; 653]).is_none());
    }
}
