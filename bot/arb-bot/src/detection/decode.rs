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

// ---------------------------------------------------------------------------
// Raydium CP-Swap (CPMM). VERIFIED 2026-06-23 against the canonical `PoolState`/`AmmConfig`
// structs (raydium-io/raydium-cp-swap) AND a real mainnet pool: PoolState `token_1_mint@200`
// decoded to WSOL and `AmmConfig.trade_fee_rate@12` = 2500 (= 0.25%, denom 1_000_000). Unlike
// Whirlpool, the reserves are NOT in PoolState — they are the two vault token-account
// `amount`s (offset 64), and the fee lives in the separate `AmmConfig` account.
// ---------------------------------------------------------------------------

/// Raydium CP-Swap `PoolState` Anchor discriminator (`sha256("account:PoolState")[..8]`),
/// confirmed against a real mainnet pool.
pub const RAYDIUM_CPMM_POOLSTATE_DISCRIMINATOR: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
/// Raydium CP-Swap `AmmConfig` Anchor discriminator (`sha256("account:AmmConfig")[..8]`).
pub const RAYDIUM_AMMCONFIG_DISCRIMINATOR: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];
/// Raydium CP-Swap fee denominator (`trade_fee_rate` is out of 1_000_000).
pub const RAYDIUM_CPMM_FEE_DENOMINATOR: u64 = 1_000_000;

/// `PoolState` field offsets (from account start, incl. the 8-byte discriminator). VERIFIED.
pub mod raydium_cpmm_offsets {
    /// `amm_config: Pubkey` (holds the fee).
    pub const AMM_CONFIG: usize = 8;
    /// `token_0_vault: Pubkey`.
    pub const TOKEN_0_VAULT: usize = 72;
    /// `token_1_vault: Pubkey`.
    pub const TOKEN_1_VAULT: usize = 104;
    /// `token_0_mint: Pubkey`.
    pub const TOKEN_0_MINT: usize = 168;
    /// `token_1_mint: Pubkey`.
    pub const TOKEN_1_MINT: usize = 200;
    /// `mint_0_decimals: u8`.
    pub const MINT_0_DECIMALS: usize = 331;
    /// `mint_1_decimals: u8`.
    pub const MINT_1_DECIMALS: usize = 332;
    /// `AmmConfig.trade_fee_rate: u64` (after bump@8 + disable_create@9 + index@10..12).
    pub const AMM_CONFIG_TRADE_FEE_RATE: usize = 12;
}

/// Decoded Raydium CP-Swap `PoolState` (the routing/assembly subset). Reserves come from the
/// vault balances; the fee comes from [`decode_raydium_amm_config_trade_fee_rate`] over the
/// `amm_config` account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaydiumCpmmPool {
    pub amm_config: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub mint_0_decimals: u8,
    pub mint_1_decimals: u8,
}

impl RaydiumCpmmPool {
    /// Assemble [`CpmmReserves`] from the two vault token-accounts (`amount@64`) + the pool's
    /// `trade_fee_rate` (from its `amm_config`). `reserve_a` is token_0, `reserve_b` is token_1.
    pub fn reserves(
        &self,
        vault_0_data: &[u8],
        vault_1_data: &[u8],
        trade_fee_rate: u64,
    ) -> Option<CpmmReserves> {
        cpmm_reserves_from_vaults(
            vault_0_data,
            vault_1_data,
            trade_fee_rate,
            RAYDIUM_CPMM_FEE_DENOMINATOR,
        )
    }
}

/// Decode a Raydium CP-Swap `PoolState`. `None` on wrong/short discriminator or truncation.
pub fn decode_raydium_cpmm_pool(data: &[u8]) -> Option<RaydiumCpmmPool> {
    if !has_anchor_discriminator(data, &RAYDIUM_CPMM_POOLSTATE_DISCRIMINATOR) {
        return None;
    }
    use raydium_cpmm_offsets as o;
    Some(RaydiumCpmmPool {
        amm_config: read_pubkey(data, o::AMM_CONFIG)?,
        token_0_vault: read_pubkey(data, o::TOKEN_0_VAULT)?,
        token_1_vault: read_pubkey(data, o::TOKEN_1_VAULT)?,
        token_0_mint: read_pubkey(data, o::TOKEN_0_MINT)?,
        token_1_mint: read_pubkey(data, o::TOKEN_1_MINT)?,
        mint_0_decimals: *data.get(o::MINT_0_DECIMALS)?,
        mint_1_decimals: *data.get(o::MINT_1_DECIMALS)?,
    })
}

/// Read `trade_fee_rate` (out of [`RAYDIUM_CPMM_FEE_DENOMINATOR`]) from a Raydium CP-Swap
/// `AmmConfig` account. `None` on wrong/short discriminator or truncation.
pub fn decode_raydium_amm_config_trade_fee_rate(data: &[u8]) -> Option<u64> {
    if !has_anchor_discriminator(data, &RAYDIUM_AMMCONFIG_DISCRIMINATOR) {
        return None;
    }
    let off = raydium_cpmm_offsets::AMM_CONFIG_TRADE_FEE_RATE;
    Some(u64::from_le_bytes(
        data.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
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

    #[test]
    fn decodes_real_raydium_cpmm_pool() {
        // Real mainnet CP-Swap pool 7e6L4dknXXVjHmnqDFmnGV8c4y9fePccsvjZEgaAPYiU (token/WSOL)
        // + its AmmConfig D4FPEr…, frozen getAccountInfo snapshots (2026-06-23). The
        // detection-3 "validated against real cloned account bytes" gate for Raydium CPMM.
        let pool = include_bytes!("fixtures/raydium_cpmm_pool_7e6L4d.bin");
        assert_eq!(&pool[0..8], &RAYDIUM_CPMM_POOLSTATE_DISCRIMINATOR);
        let p = decode_raydium_cpmm_pool(pool).expect("cpmm pool decodes");
        assert_eq!(
            p.token_1_mint,
            "So11111111111111111111111111111111111111112"
                .parse()
                .unwrap()
        );
        assert_eq!(
            p.token_0_mint,
            "LUC6TxSNr1yodP8jbox4fpVoEwRhV9ZkczZV4uZ6yce"
                .parse()
                .unwrap()
        );
        assert_eq!(
            p.amm_config,
            "D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2"
                .parse()
                .unwrap()
        );
        assert_eq!(
            p.token_0_vault,
            "57B4hPwTmnqjMYtpazDNizciRJb4B7kU8c28sbVu4jTq"
                .parse()
                .unwrap()
        );
        assert_eq!(
            p.token_1_vault,
            "4mpr2XXc5ay4UJJ7fheGDn29k5tqTQn5Zp6d6k6pZBNo"
                .parse()
                .unwrap()
        );
        assert_eq!(p.mint_0_decimals, 9);
        assert_eq!(p.mint_1_decimals, 9);

        // AmmConfig carries the swap fee: trade_fee_rate = 2500 / 1_000_000 = 0.25%.
        let cfg = include_bytes!("fixtures/raydium_cpmm_ammconfig_D4FPEr.bin");
        assert_eq!(&cfg[0..8], &RAYDIUM_AMMCONFIG_DISCRIMINATOR);
        let fee = decode_raydium_amm_config_trade_fee_rate(cfg).expect("fee decodes");
        assert_eq!(fee, 2500);
        assert_eq!(RAYDIUM_CPMM_FEE_DENOMINATOR, 1_000_000);

        // Wrong disc => None on both decoders.
        assert!(decode_raydium_cpmm_pool(&[0u8; 637]).is_none());
        assert!(decode_raydium_amm_config_trade_fee_rate(&[0u8; 236]).is_none());
    }
}
