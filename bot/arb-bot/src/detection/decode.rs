//! Per-venue account decoding (detection-3). Turns a raw streamed account into the structural
//! fields the pool-state assembly needs: vault / mint / config pubkeys for the constant-product
//! venues (Raydium CPMM, PumpSwap — reserves are read from the two vault token-accounts) and the
//! in-account sqrt-price/liquidity/tick fields for Orca Whirlpool.
//!
//! Every field OFFSET and account DISCRIMINATOR below was verified three ways and is locked by
//! the real-mainnet-byte fixtures in the test module:
//!   1. cumulative byte arithmetic over the authoritative Anchor struct (field order + sizes),
//!   2. an independent adversarial re-derivation from a second source (IDL / decoder lib),
//!   3. live `getAccountInfo` decode of a known mainnet pool of each venue (Chainstack RPC,
//!      2026-06-23) — the fixtures embedded in `tests` are those exact snapshots.
//!
//! Discriminators are `sha256("account:<Name>")[..8]`; they are checked before any offset read,
//! and every read is bounds-checked (`None` on a short/!matching buffer) so a malformed or
//! spoofed update fails closed rather than mis-decoding.

use arb_math::CpmmReserves;
use solana_pubkey::Pubkey;

// ---- Anchor account discriminators (sha256("account:<Name>")[..8]) ----
/// Raydium CP-Swap `PoolState`.
pub const RAYDIUM_CPMM_POOL_DISCRIMINATOR: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
/// Raydium CP-Swap `AmmConfig` (holds the trade fee).
pub const RAYDIUM_AMM_CONFIG_DISCRIMINATOR: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];
/// Orca `Whirlpool`.
pub const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];
/// PumpSwap `Pool`.
pub const PUMPSWAP_POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
/// PumpSwap `GlobalConfig` (holds the fee basis points).
pub const PUMPSWAP_GLOBAL_CONFIG_DISCRIMINATOR: [u8; 8] = [149, 8, 156, 202, 160, 252, 176, 217];

// ---- Fee denominators (numerator / denominator = fee fraction on the input amount) ----
/// Raydium CP-Swap `FEE_RATE_DENOMINATOR_VALUE` — `trade_fee_rate` is over 1e6.
pub const RAYDIUM_CPMM_FEE_DENOMINATOR: u64 = 1_000_000;
/// Orca Whirlpool `fee_rate` is in hundredths of a basis point — over 1e6.
pub const WHIRLPOOL_FEE_DENOMINATOR: u64 = 1_000_000;
/// PumpSwap fees are quoted in basis points — over 1e4.
pub const PUMPSWAP_FEE_DENOMINATOR: u64 = 10_000;

/// Verified byte offsets (from start of account data, incl. the 8-byte discriminator).
pub mod offsets {
    /// Raydium CP-Swap `PoolState` (637 bytes).
    pub mod raydium_cpmm_pool {
        pub const AMM_CONFIG: usize = 8;
        pub const TOKEN_0_VAULT: usize = 72;
        pub const TOKEN_1_VAULT: usize = 104;
        pub const TOKEN_0_MINT: usize = 168;
        pub const TOKEN_1_MINT: usize = 200;
        pub const TOKEN_0_PROGRAM: usize = 232;
        pub const TOKEN_1_PROGRAM: usize = 264;
        pub const STATUS: usize = 329;
        pub const MINT_0_DECIMALS: usize = 331;
        pub const MINT_1_DECIMALS: usize = 332;
        pub const LEN: usize = 637;
    }
    /// Raydium CP-Swap `AmmConfig` (236 bytes).
    pub mod raydium_amm_config {
        pub const TRADE_FEE_RATE: usize = 12;
        pub const LEN: usize = 236;
    }
    /// Orca `Whirlpool` (653 bytes).
    pub mod whirlpool {
        pub const TICK_SPACING: usize = 41;
        pub const FEE_RATE: usize = 45;
        pub const LIQUIDITY: usize = 49;
        pub const SQRT_PRICE: usize = 65;
        pub const TICK_CURRENT_INDEX: usize = 81;
        pub const TOKEN_MINT_A: usize = 101;
        pub const TOKEN_VAULT_A: usize = 133;
        pub const TOKEN_MINT_B: usize = 181;
        pub const TOKEN_VAULT_B: usize = 213;
        pub const LEN: usize = 653;
    }
    /// PumpSwap `Pool`. Trailing length varies across program upgrades; only the fields through
    /// `COIN_CREATOR` are stable, so the decoder requires at least `MIN_LEN`.
    pub mod pumpswap_pool {
        pub const CREATOR: usize = 11;
        pub const BASE_MINT: usize = 43;
        pub const QUOTE_MINT: usize = 75;
        pub const LP_MINT: usize = 107;
        pub const POOL_BASE_TOKEN_ACCOUNT: usize = 139;
        pub const POOL_QUOTE_TOKEN_ACCOUNT: usize = 171;
        pub const COIN_CREATOR: usize = 211;
        pub const MIN_LEN: usize = COIN_CREATOR + 32;
    }
    /// PumpSwap `GlobalConfig`.
    pub mod pumpswap_global_config {
        pub const LP_FEE_BPS: usize = 40;
        pub const PROTOCOL_FEE_BPS: usize = 48;
        pub const COIN_CREATOR_FEE_BPS: usize = 313;
        pub const MIN_LEN: usize = COIN_CREATOR_FEE_BPS + 8;
    }
}

// ---- bounds-checked little-endian readers ----
fn read_pubkey(data: &[u8], offset: usize) -> Option<Pubkey> {
    let arr: [u8; 32] = data.get(offset..offset + 32)?.try_into().ok()?;
    Some(Pubkey::new_from_array(arr))
}
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn read_i32(data: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}
fn read_u128(data: &[u8], offset: usize) -> Option<u128> {
    Some(u128::from_le_bytes(
        data.get(offset..offset + 16)?.try_into().ok()?,
    ))
}

/// SPL/Token-2022 token-account `amount` (u64 LE) at offset 64 — the vault reserve. The base
/// `Account` layout is identical for Token and Token-2022 (extensions follow the base), so this
/// reads both. (Vaults are token-accounts, NOT Anchor accounts — no discriminator.)
pub fn read_vault_amount(data: &[u8]) -> Option<u64> {
    read_u64(data, 64)
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

// ---- Raydium CP-Swap ----

/// Decoded Raydium CP-Swap `PoolState` (the structural pubkeys + flags; reserves come from the
/// two vault token-accounts, the fee from the `amm_config` account).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaydiumCpmmPool {
    pub amm_config: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub status: u8,
    pub mint_0_decimals: u8,
    pub mint_1_decimals: u8,
}

impl RaydiumCpmmPool {
    /// `status` is a bitfield; bit 2 (value 4) set ⇒ swaps disabled.
    pub const STATUS_SWAP_DISABLED_BIT: u8 = 0b100;

    /// True iff the pool currently permits swaps (bit 2 clear).
    pub fn swap_enabled(&self) -> bool {
        self.status & Self::STATUS_SWAP_DISABLED_BIT == 0
    }
}

/// Decode a Raydium CP-Swap `PoolState`. `None` on wrong discriminator or short buffer.
pub fn decode_raydium_cpmm_pool(data: &[u8]) -> Option<RaydiumCpmmPool> {
    use offsets::raydium_cpmm_pool as o;
    if !has_anchor_discriminator(data, &RAYDIUM_CPMM_POOL_DISCRIMINATOR) {
        return None;
    }
    Some(RaydiumCpmmPool {
        amm_config: read_pubkey(data, o::AMM_CONFIG)?,
        token_0_vault: read_pubkey(data, o::TOKEN_0_VAULT)?,
        token_1_vault: read_pubkey(data, o::TOKEN_1_VAULT)?,
        token_0_mint: read_pubkey(data, o::TOKEN_0_MINT)?,
        token_1_mint: read_pubkey(data, o::TOKEN_1_MINT)?,
        token_0_program: read_pubkey(data, o::TOKEN_0_PROGRAM)?,
        token_1_program: read_pubkey(data, o::TOKEN_1_PROGRAM)?,
        status: *data.get(o::STATUS)?,
        mint_0_decimals: *data.get(o::MINT_0_DECIMALS)?,
        mint_1_decimals: *data.get(o::MINT_1_DECIMALS)?,
    })
}

/// Raydium CP-Swap trade-fee numerator (denominator = [`RAYDIUM_CPMM_FEE_DENOMINATOR`]), read
/// from the pool's separate `AmmConfig` account. `None` on wrong discriminator or short buffer.
pub fn decode_raydium_amm_config_trade_fee_rate(data: &[u8]) -> Option<u64> {
    if !has_anchor_discriminator(data, &RAYDIUM_AMM_CONFIG_DISCRIMINATOR) {
        return None;
    }
    read_u64(data, offsets::raydium_amm_config::TRADE_FEE_RATE)
}

// ---- Orca Whirlpool ----

/// Decoded Orca `Whirlpool`. `sqrt_price` (Q64.64) and `liquidity` live in-account; reserves can
/// also be read from the two vault token-accounts for the constant-product approximation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Whirlpool {
    pub tick_spacing: u16,
    pub fee_rate: u16,
    pub liquidity: u128,
    pub sqrt_price: u128,
    pub tick_current_index: i32,
    pub token_mint_a: Pubkey,
    pub token_vault_a: Pubkey,
    pub token_mint_b: Pubkey,
    pub token_vault_b: Pubkey,
}

/// Decode an Orca `Whirlpool`. `None` on wrong discriminator or short buffer.
pub fn decode_whirlpool(data: &[u8]) -> Option<Whirlpool> {
    use offsets::whirlpool as o;
    if !has_anchor_discriminator(data, &WHIRLPOOL_DISCRIMINATOR) {
        return None;
    }
    Some(Whirlpool {
        tick_spacing: read_u16(data, o::TICK_SPACING)?,
        fee_rate: read_u16(data, o::FEE_RATE)?,
        liquidity: read_u128(data, o::LIQUIDITY)?,
        sqrt_price: read_u128(data, o::SQRT_PRICE)?,
        tick_current_index: read_i32(data, o::TICK_CURRENT_INDEX)?,
        token_mint_a: read_pubkey(data, o::TOKEN_MINT_A)?,
        token_vault_a: read_pubkey(data, o::TOKEN_VAULT_A)?,
        token_mint_b: read_pubkey(data, o::TOKEN_MINT_B)?,
        token_vault_b: read_pubkey(data, o::TOKEN_VAULT_B)?,
    })
}

// ---- PumpSwap ----

/// Decoded PumpSwap `Pool` (structural pubkeys; reserves come from the two pool token-accounts,
/// fees from the `GlobalConfig` account).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpSwapPool {
    pub creator: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub pool_base_token_account: Pubkey,
    pub pool_quote_token_account: Pubkey,
    pub coin_creator: Pubkey,
}

/// Decode a PumpSwap `Pool`. `None` on wrong discriminator or short buffer.
pub fn decode_pumpswap_pool(data: &[u8]) -> Option<PumpSwapPool> {
    use offsets::pumpswap_pool as o;
    if !has_anchor_discriminator(data, &PUMPSWAP_POOL_DISCRIMINATOR) {
        return None;
    }
    Some(PumpSwapPool {
        creator: read_pubkey(data, o::CREATOR)?,
        base_mint: read_pubkey(data, o::BASE_MINT)?,
        quote_mint: read_pubkey(data, o::QUOTE_MINT)?,
        lp_mint: read_pubkey(data, o::LP_MINT)?,
        pool_base_token_account: read_pubkey(data, o::POOL_BASE_TOKEN_ACCOUNT)?,
        pool_quote_token_account: read_pubkey(data, o::POOL_QUOTE_TOKEN_ACCOUNT)?,
        coin_creator: read_pubkey(data, o::COIN_CREATOR)?,
    })
}

/// PumpSwap total swap fee in basis points (denominator = [`PUMPSWAP_FEE_DENOMINATOR`]), summed
/// from the global config: `lp_fee + protocol_fee + coin_creator_fee`. `None` on wrong
/// discriminator, short buffer, or overflow.
pub fn decode_pumpswap_total_fee_bps(global_config_data: &[u8]) -> Option<u64> {
    use offsets::pumpswap_global_config as o;
    if !has_anchor_discriminator(global_config_data, &PUMPSWAP_GLOBAL_CONFIG_DISCRIMINATOR) {
        return None;
    }
    let lp = read_u64(global_config_data, o::LP_FEE_BPS)?;
    let protocol = read_u64(global_config_data, o::PROTOCOL_FEE_BPS)?;
    let coin_creator = read_u64(global_config_data, o::COIN_CREATOR_FEE_BPS)?;
    lp.checked_add(protocol)?.checked_add(coin_creator)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- existing primitive tests ----

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

    // ---- fail-closed edge cases ----

    #[test]
    fn decoders_reject_wrong_discriminator_and_short_buffers() {
        let zero = [0u8; 700];
        assert!(decode_raydium_cpmm_pool(&zero).is_none());
        assert!(decode_whirlpool(&zero).is_none());
        assert!(decode_pumpswap_pool(&zero).is_none());
        assert!(decode_raydium_amm_config_trade_fee_rate(&zero).is_none());
        assert!(decode_pumpswap_total_fee_bps(&zero).is_none());

        // Right discriminator but a buffer too short to hold the fields -> None, never panic.
        let mut tiny = vec![0u8; 8];
        tiny.copy_from_slice(&RAYDIUM_CPMM_POOL_DISCRIMINATOR);
        assert!(decode_raydium_cpmm_pool(&tiny).is_none());
        let mut tiny_wp = vec![0u8; 8];
        tiny_wp.copy_from_slice(&WHIRLPOOL_DISCRIMINATOR);
        assert!(decode_whirlpool(&tiny_wp).is_none());
    }

    // ---- real-mainnet-byte fixtures (Chainstack getAccountInfo, 2026-06-23) ----
    //
    // These freeze a live account of each venue and decode it at the verified offsets, locking
    // both the discriminator and every field offset against ground truth. Pubkeys are stable;
    // scalar fields (sqrt_price/liquidity/fees/decimals) are the snapshot's exact values.

    /// Minimal, dependency-free standard base64 decoder (test-only).
    fn b64(s: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut rev = [255u8; 256];
        for (i, &c) in ALPHABET.iter().enumerate() {
            rev[c as usize] = i as u8;
        }
        let mut out = Vec::new();
        let (mut buf, mut bits) = (0u32, 0u32);
        for &c in s.as_bytes() {
            if c == b'=' {
                break;
            }
            let v = rev[c as usize];
            if v == 255 {
                continue; // skip any whitespace
            }
            buf = (buf << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        out
    }

    // Orca Whirlpool SOL/USDC (tick-spacing 4) — Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE
    const WHIRLPOOL_SOL_USDC: &str = "P5XRDOGAYwkT5EH4ORPKaLBjT7Al/eqohzfoQRDRJV41ezN33e4czf8EAAQAkAEUBceq8S8UcwIAAAAAAAAAAABZntukYcJDRQAAAAAAAAAA3Zn//1gmDgMAAAAA4RyjAQAAAAAGm4hX/quBhPtof2NGGMA12sQ53BrrO1WYoPAAAAAAAchN8kM4mDvkqFswl7r0C8lXEQjSiawAs2jfF11Edc963YxkRuWoRbEAAAAAAAAAAMb6evO+2606PWXzaqvJdDGxu+TC0vbg5HymAgNFL11hFl+VcsWpaqUC3VEQVKJqbSWO98HW1sGu4SkZFNxRAjJJkqOU0xHwFgAAAAAAAAAAmHY5agAAAAAMANCv64YU2n8Zq6AtQPGMaSWF9lAg387T1eX5qcDE4Q8bkJQIzrVDfhKReyB9qZTQ6FenQB4SLAPfa/fG1/wqui6/LwKaI7GKR1R798LZ7CubYjLuw+NoR9dh+omDPGYAAAAAAAAAAAAAAAAAAAAAIxHh3tFPDkQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    // Raydium CP-Swap PoolState WSOL/CLNS — FmNz8z2oN4CheHj4EUXVXC6doFN4RRmhGwxFQgrJVJFs
    const RAYDIUM_CPMM_POOL: &str = "9+3j9dfD3kYYqU87qGd9BoiPIo/FUcw+QszFrbtpttXgwSH4xgBKOIeDzdk2DWWVf4SVivQiBn/uNQhK3rOWBaFPmesTkI0VZGGbD4pIZFbJyfoeI3Ullysm0L/RgSx5SVqw81SpbQiDpbGIwDU2wZJ38meocZ6xHrSou1NcKx8k7nqapG4wW42VWX+JLdwGrrIv7pj8UoW2LA6LBQM8/kVTKUSMb/nhBpuIV/6rgYT7aH9jRhjANdrEOdwa6ztVmKDwAAAAAAGogndEyk5zgZr3oVX9piz0to77Jg5F/XjN3kZ2B1+YkAbd9uHXZaGT2cvhRs7reawctIXtX1s3kTqM9YV+/wCpBt324ddloZPZy+FGzut5rBy0he1fWzeROoz1hX7/AKnFCMdJAbfrH/vOkia/cKFLSYTAGlOdRGL2ceESH9kJ0v0ACQkJZAAAAAAAAACYxREAAAAAAGrApHDW8AEAiOwFAAAAAADL6jbQnKUAAGyNo2cAAAAAFAMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

    // Raydium CP-Swap AmmConfig (index 2, 2.0% fee) — 2fGXL8uhqxJ4tpgtosHZXT4zcQap6j62z3bMDxdkMvy5
    const RAYDIUM_AMM_CONFIG: &str = "2vQhaMvLK2//AAIAIE4AAAAAAADA1AEAAAAAAECcAAAAAAAAgNHwCAAAAAAF2xU6D/SH9pzeiQAf7ExGlbbUIV6iRLh71Otyh/hXqNcGSjj20XsRPl9m3PtjuHN//OJkSr8KALmLNDVtY4WD9AEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    // PumpSwap Pool (pump token / WSOL) — 118Kjv7HGmoVEnXRupC43AaB2aaAyitKHaW1Kdz1SJp
    const PUMPSWAP_POOL: &str = "8ZptBBGxbbz+AADqEgHvFDWaIXp00QRa9tw+rcbLDcGD4kVcGejFAupGoAbXyM6881SmtQeprXy6NnFVNKIVZ7O+woUCbvxqKTzfBpuIV/6rgYT7aH9jRhjANdrEOdwa6ztVmKDwAAAAAAFu8VBWrDzuAHPMWRR0xu92FdTsndPHZTHLK59hRz/phUHI2jfnrRQ6dmfk8bPFKxsmXZErXQBE2HzbXqmHnUL+GFemJtYZrw4U3qPyXRynBtgjdooCw3Wz0VhAEr4+nweUT5IAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    // PumpSwap GlobalConfig (lp 20 / protocol 5 / coin-creator 5 bps) — ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw
    const PUMPSWAP_GLOBAL_CONFIG: &str = "lQicyqD8sNnTu4yrNBzgUoRX8sOBfTJ4RBlj3NVf7Vi6JMmZ3awCqhQAAAAAAAAABQAAAAAAAAAASsL40N1cvJfjKJwZfLUGKlTz2Va5zm5RFfllZ6pcs+ZgjMwd/OlhtDt3nBkVBabi079F1aTbRhitdsgtYXVFNWODcwAOoiyyZNNK/2SgS176v7t03c0EiZexmBVH19EQg4R0KS5nWpS0NuywqZiJQjKKg93GIzgClhJnxc1hF8uNGBoMhJ+pN6bzSt7TCB75VwCqywybs9kJpLkUdSek69eqj7Bg2CkbTE1HXa/3Yslr3A2s6zbAEurRLtOpSEFh4ATIfOuY+lzkf4A4Bv0seUXSlSSVmuwA3tl4FPOPeEb/g4OBi6j6KMPNO21ek/n6uPCXm8NyFazFskaHe6jDyQUAAAAAAAAAByFdmUB5NpThFgZs5Fm4GP35u6DHtBt4P6OhIMpBlTKii1/SarR5pqnMbL9rCyPrYYhaNx4BIKypE77vPROKeOiTFB+xjp8VdNgQ4XjhnjBgTjF1qi5KMt/IYAcn0QcJATWEU2JWCU+RKBkSfvpORGtDM3IXk9E4dvmq2/PcfQtfbnUBgiD5QmdwAyN7TWtFN1m0pcaQtZw12bsYegkMvSozmHqeuxNnmatZklsT5dyLMIHfAF20J8FHj6Rv+MNHoHTpVD8+N6LQRiJ63ctOnHdMRCWMQ+3ySqiq4fACFGZb2kw4zW23Q49ZtAi7nsO0yp6K0fHyRlPEmbV5bCDb+bMt3Z7qPzmzchFccYR8GEXPpTbGhQdOAw5E0CHePvnjXEy3gPCO4v7oS+xEald4Jdpo1Dn6il2jsMXP9Q9j9FRrAUOeZRDAPWX62THonQS+C7cNUZcfUcQV+zRMB9tBnyEiAiNVFqkXE0xnWIxJOCCuFV7pZmVXesG3GNpH3c8qBQ7mp+IgaLuIZAqlf5CTCMYf73EaAWP1p1XAcLyGDR9jZyB87NpbzGyx6vDxbWhARWaxjVbSSBrLMXAyZW6QVRx4RJZB+ElY3HOnaoXYdW9VwCzayom6GTJ5DDaKsVfpLXPFS5a1yTGUHkbqS+Lg4xEndE/Gt0z7RV7+r4vVcXks7UT8H3j5SjPQkJxea1+wIVcK2Nutjej9s9IO0c2Z645Oh3AVfuvrZ4plXbmbN/axMmx2V9uQz7ioer74x7byyGmIEwAAAAAAAA==";

    #[test]
    fn whirlpool_fixture_decodes_sol_usdc() {
        let data = b64(WHIRLPOOL_SOL_USDC);
        assert_eq!(data.len(), offsets::whirlpool::LEN);
        let w = decode_whirlpool(&data).expect("whirlpool decodes");
        assert_eq!(w.tick_spacing, 4);
        assert_eq!(w.fee_rate, 400); // 0.04% over 1e6
        assert_eq!(w.liquidity, 689_480_494_328_519);
        assert_eq!(w.sqrt_price, 4_991_046_536_690_114_137);
        assert_eq!(w.tick_current_index, -26_147);
        assert_eq!(
            w.token_mint_a.to_string(),
            "So11111111111111111111111111111111111111112" // WSOL
        );
        assert_eq!(
            w.token_mint_b.to_string(),
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" // USDC
        );
        assert_eq!(
            w.token_vault_a.to_string(),
            "EUuUbDcafPrmVTD5M6qoJAoyyNbihBhugADAxRMn5he9"
        );
        assert_eq!(
            w.token_vault_b.to_string(),
            "2WLWEuKDgkDUccTpbwYp1GToYktiSB1cXvreHUwiSUVP"
        );
        // sqrt_price -> price, decimal-adjusted (9 base, 6 quote) is a sane SOL/USDC level.
        let usdc_per_sol = sqrt_price_x64_to_price(w.sqrt_price) * 1_000.0;
        assert!((20.0..2_000.0).contains(&usdc_per_sol), "{usdc_per_sol}");
    }

    #[test]
    fn raydium_cpmm_pool_fixture_decodes() {
        let data = b64(RAYDIUM_CPMM_POOL);
        assert_eq!(data.len(), offsets::raydium_cpmm_pool::LEN);
        let p = decode_raydium_cpmm_pool(&data).expect("cpmm pool decodes");
        assert_eq!(
            p.amm_config.to_string(),
            "2fGXL8uhqxJ4tpgtosHZXT4zcQap6j62z3bMDxdkMvy5"
        );
        assert_eq!(
            p.token_0_mint.to_string(),
            "So11111111111111111111111111111111111111112" // WSOL
        );
        assert_eq!(
            p.token_1_mint.to_string(),
            "CLnsvL118rFABjjpGkR5jY8JeUib3SjEf4Eyu7ZVhbKu"
        );
        assert_eq!(
            p.token_0_vault.to_string(),
            "7kr6P33VCitwmwpShpUwXKpjnnXQkVyNZNqLktYKRKm9"
        );
        assert_eq!(
            p.token_1_vault.to_string(),
            "9rtx1zk5rSXFjg9oDKhzTnJWeVhuA8CJFvvgdKppsz8a"
        );
        // Both legs are classic SPL Token here.
        assert_eq!(
            p.token_0_program.to_string(),
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        );
        assert_eq!(p.mint_0_decimals, 9); // WSOL
        assert_eq!(p.mint_1_decimals, 9);
        assert_eq!(p.status, 0);
        assert!(p.swap_enabled());
    }

    #[test]
    fn raydium_amm_config_fixture_decodes_fee() {
        let data = b64(RAYDIUM_AMM_CONFIG);
        assert_eq!(data.len(), offsets::raydium_amm_config::LEN);
        let fee = decode_raydium_amm_config_trade_fee_rate(&data).expect("amm config decodes");
        assert_eq!(fee, 20_000); // 20000 / 1_000_000 = 2.0%
        assert_eq!(RAYDIUM_CPMM_FEE_DENOMINATOR, 1_000_000);
    }

    #[test]
    fn pumpswap_pool_fixture_decodes() {
        let data = b64(PUMPSWAP_POOL);
        assert!(data.len() >= offsets::pumpswap_pool::MIN_LEN);
        let p = decode_pumpswap_pool(&data).expect("pumpswap pool decodes");
        assert_eq!(
            p.base_mint.to_string(),
            "TiHeVgMZzWpbWBSoavxkEkZTWiCSxcFS6gvYipYpump"
        );
        assert_eq!(
            p.quote_mint.to_string(),
            "So11111111111111111111111111111111111111112" // WSOL (canonical graduation quote)
        );
        assert_eq!(
            p.pool_base_token_account.to_string(),
            "5Ro8qyhb1nnD3AGQiheFpvoh9pZvDbRDsAeCRFEXmcCu"
        );
        assert_eq!(
            p.pool_quote_token_account.to_string(),
            "2e2JWuRTPPh4D8zbb6L1hsyKzxieyqkqSsAq93Bnq6Z4"
        );
    }

    #[test]
    fn pumpswap_global_config_fixture_decodes_fee() {
        let data = b64(PUMPSWAP_GLOBAL_CONFIG);
        assert!(data.len() >= offsets::pumpswap_global_config::MIN_LEN);
        let bps = decode_pumpswap_total_fee_bps(&data).expect("global config decodes");
        assert_eq!(bps, 30); // 20 lp + 5 protocol + 5 coin-creator
        assert_eq!(PUMPSWAP_FEE_DENOMINATOR, 10_000);
    }
}
