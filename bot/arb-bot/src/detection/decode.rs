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

// ============================================================================
// detection-11 — Fase 2.5 venues (gated by M1-GATE-EXT): Meteora DLMM, Meteora DAMM v2,
// Raydium CLMM.
//
// Every offset / discriminator / LEN below is from the `fase25-venue-research` workflow
// (2026-06-23): each was cross-checked against the canonical on-chain Rust struct, the official
// IDL, AND a live mainnet `getAccountInfo` (Chainstack), then adversarially second-sourced.
//
// ⚠️ TWO Anchor discriminators COLLIDE by struct name across programs (the 8-byte tag is
// `sha256("account:<Name>")`, which depends only on the type name, not the program):
//   * Raydium CLMM `PoolState` shares [247,237,227,245,215,195,222,70] with Raydium CPMM.
//   * Meteora DAMM v2 `Pool` shares [241,154,109,4,17,177,109,188] with PumpSwap.
// The discriminator alone CANNOT disambiguate — callers route by the account's OWNER program id.
// Each decoder additionally pins the venue's exact data LEN as a secondary guard (CPMM 637 vs
// CLMM 1544; DAMM v2 1112 vs PumpSwap variable), so a CPMM/PumpSwap buffer fed to the wrong
// decoder fails the length check rather than mis-decoding.
// ============================================================================

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

// ---- Meteora DLMM (`lb_clmm::LbPair`) ----
/// Meteora DLMM `LbPair` account discriminator (`sha256("account:LbPair")[..8]`).
pub const DLMM_LB_PAIR_DISCRIMINATOR: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
/// Meteora DLMM `BinArray` account discriminator (the swap walks these for liquidity).
pub const DLMM_BIN_ARRAY_DISCRIMINATOR: [u8; 8] = [92, 142, 92, 220, 5, 148, 70, 181];
/// DLMM total-fee-rate denominator (`FEE_PRECISION`): rates are over 1e9 (1e9 == 100%).
pub const DLMM_FEE_DENOMINATOR: u64 = 1_000_000_000;
/// DLMM price-ratio bps denominator: adjacent bins differ by `1 + bin_step/10000`.
pub const DLMM_BASIS_POINT_MAX: u64 = 10_000;

/// Verified `LbPair` byte offsets (absolute, incl. the 8-byte discriminator). The
/// `StaticParameters` fields use the research-verdict-CORRECTED absolute offsets (IDL relative
/// layout + base 8), NOT the spec's off-by-8 "convenience" cluster.
pub mod dlmm_lb_pair_offsets {
    pub const BASE_FACTOR: usize = 8; // u16  (StaticParameters.base_factor)
    pub const PROTOCOL_SHARE: usize = 32; // u16
    pub const BASE_FEE_POWER_FACTOR: usize = 34; // u8
    pub const COLLECT_FEE_MODE: usize = 36; // u8 (0=InputOnly, 1=OnlyY)
    pub const ACTIVE_ID: usize = 76; // i32 (current price pointer)
    pub const BIN_STEP: usize = 80; // u16 (bps)
    pub const STATUS: usize = 82; // u8 (0=Enabled)
    pub const TOKEN_X_MINT: usize = 88;
    pub const TOKEN_Y_MINT: usize = 120;
    pub const RESERVE_X: usize = 152; // X vault token-account
    pub const RESERVE_Y: usize = 184; // Y vault token-account
    pub const TOKEN_MINT_X_PROGRAM_FLAG: usize = 880; // u8 (0=SPL, 1=Token2022)
    pub const TOKEN_MINT_Y_PROGRAM_FLAG: usize = 881; // u8
    pub const LEN: usize = 904;
}

/// Decoded Meteora DLMM `LbPair` (the swap/price-relevant fields). The active bin's per-bin
/// reserves come from the `BinArray` accounts, not this struct; `active_id` + `bin_step` give the
/// current bin price `(1 + bin_step/10000)^active_id`, and `base_factor`/`bin_step` give the base
/// fee. The variable (volatility) fee is execution-time-clock-dependent and lives in the quoter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlmmLbPair {
    pub base_factor: u16,
    pub protocol_share: u16,
    pub base_fee_power_factor: u8,
    /// 0 = fee on input (InputOnly), 1 = fee on the Y token (OnlyY).
    pub collect_fee_mode: u8,
    pub active_id: i32,
    pub bin_step: u16,
    pub status: u8,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub token_x_is_token2022: bool,
    pub token_y_is_token2022: bool,
}

impl DlmmLbPair {
    /// True iff swaps are enabled (`status == 0`).
    pub fn swap_enabled(&self) -> bool {
        self.status == 0
    }

    /// The DLMM **base** fee rate over [`DLMM_FEE_DENOMINATOR`] (1e9):
    /// `base_factor * bin_step * 10 * 10^base_fee_power_factor`. The total on-chain fee adds a
    /// volatility-dependent variable component (see the quoter); this is the static floor.
    pub fn base_fee_rate(&self) -> Option<u64> {
        let bin_step = self.bin_step as u64;
        let base = (self.base_factor as u64)
            .checked_mul(bin_step)?
            .checked_mul(10)?;
        let pow = 10u64.checked_pow(self.base_fee_power_factor as u32)?;
        base.checked_mul(pow)
    }
}

/// Decode a Meteora DLMM `LbPair`. `None` on wrong discriminator or wrong/short buffer.
pub fn decode_dlmm_lb_pair(data: &[u8]) -> Option<DlmmLbPair> {
    use dlmm_lb_pair_offsets as o;
    if !has_anchor_discriminator(data, &DLMM_LB_PAIR_DISCRIMINATOR) {
        return None;
    }
    if data.len() != o::LEN {
        return None; // exact-LEN guard disambiguates the (none here, but consistent) collisions
    }
    Some(DlmmLbPair {
        base_factor: read_u16(data, o::BASE_FACTOR)?,
        protocol_share: read_u16(data, o::PROTOCOL_SHARE)?,
        base_fee_power_factor: *data.get(o::BASE_FEE_POWER_FACTOR)?,
        collect_fee_mode: *data.get(o::COLLECT_FEE_MODE)?,
        active_id: read_i32(data, o::ACTIVE_ID)?,
        bin_step: read_u16(data, o::BIN_STEP)?,
        status: *data.get(o::STATUS)?,
        token_x_mint: read_pubkey(data, o::TOKEN_X_MINT)?,
        token_y_mint: read_pubkey(data, o::TOKEN_Y_MINT)?,
        reserve_x: read_pubkey(data, o::RESERVE_X)?,
        reserve_y: read_pubkey(data, o::RESERVE_Y)?,
        token_x_is_token2022: *data.get(o::TOKEN_MINT_X_PROGRAM_FLAG)? != 0,
        token_y_is_token2022: *data.get(o::TOKEN_MINT_Y_PROGRAM_FLAG)? != 0,
    })
}

// ---- Meteora DAMM v2 / CP-AMM (`cp_amm::Pool`) ----
/// Meteora DAMM v2 `Pool` account discriminator (`sha256("account:Pool")[..8]`). COLLIDES with
/// PumpSwap `Pool`; disambiguate by owner + the exact LEN guard (1112).
pub const DAMM_V2_POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
/// DAMM v2 trade-fee denominator (`FEE_DENOMINATOR`): numerators are over 1e9.
pub const DAMM_V2_FEE_DENOMINATOR: u64 = 1_000_000_000;

/// Verified `Pool` byte offsets (absolute, incl. the 8-byte discriminator).
pub mod damm_v2_pool_offsets {
    pub const CLIFF_FEE_NUMERATOR: usize = 8; // u64 (base fee at cliff, over 1e9)
    pub const BASE_FEE_MODE: usize = 16; // u8 (0=TimeLinear,1=TimeExp,2=RateLimiter,3/4=MktCap)
    pub const PROTOCOL_FEE_PERCENT: usize = 48; // u8 (split only; no user-output effect)
    pub const DYNAMIC_FEE_INITIALIZED: usize = 56; // u8 (!=0 => variable fee active)
    pub const TOKEN_A_MINT: usize = 168;
    pub const TOKEN_B_MINT: usize = 200;
    pub const TOKEN_A_VAULT: usize = 232;
    pub const TOKEN_B_VAULT: usize = 264;
    pub const LIQUIDITY: usize = 360; // u128 (L)
    pub const SQRT_MIN_PRICE: usize = 424; // u128 (band floor, A->B)
    pub const SQRT_MAX_PRICE: usize = 440; // u128 (band ceil, B->A)
    pub const SQRT_PRICE: usize = 456; // u128 (current, Q64.64)
    pub const POOL_STATUS: usize = 481; // u8 (1=disabled)
    pub const TOKEN_A_FLAG: usize = 482; // u8 (0=SPL,1=Token2022)
    pub const TOKEN_B_FLAG: usize = 483; // u8
    pub const COLLECT_FEE_MODE: usize = 484; // u8 (0=BothToken,1=OnlyB,2=Compounding)
    pub const FEE_VERSION: usize = 486; // u8 (0 => max num 5e8, 1 => 9.9e8)
    pub const LEN: usize = 1112;
}

/// Decoded Meteora DAMM v2 `Pool` — a Uniswap-V3-style concentrated-liquidity AMM with a single
/// full-range position (NOT `x*y=k`). The sqrt-price/liquidity/band fields drive the bit-exact
/// quoter; the fee fields select the base-fee scheduler + variable-fee path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DammV2Pool {
    pub cliff_fee_numerator: u64,
    pub base_fee_mode: u8,
    pub protocol_fee_percent: u8,
    pub dynamic_fee_initialized: bool,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub liquidity: u128,
    pub sqrt_min_price: u128,
    pub sqrt_max_price: u128,
    pub sqrt_price: u128,
    pub pool_status: u8,
    pub collect_fee_mode: u8,
    pub fee_version: u8,
    pub token_a_is_token2022: bool,
    pub token_b_is_token2022: bool,
}

impl DammV2Pool {
    /// True iff swaps are enabled (`pool_status != 1`).
    pub fn swap_enabled(&self) -> bool {
        self.pool_status != 1
    }

    /// Maximum total fee numerator (over [`DAMM_V2_FEE_DENOMINATOR`]) for this pool's
    /// `fee_version`: 0 ⇒ 5e8 (50%), else 9.9e8 (99%).
    pub fn max_fee_numerator(&self) -> u64 {
        if self.fee_version == 0 {
            500_000_000
        } else {
            990_000_000
        }
    }
}

/// Decode a Meteora DAMM v2 `Pool`. `None` on wrong discriminator or wrong/short buffer (the
/// exact-LEN guard rejects a PumpSwap `Pool`, which shares the discriminator).
pub fn decode_damm_v2_pool(data: &[u8]) -> Option<DammV2Pool> {
    use damm_v2_pool_offsets as o;
    if !has_anchor_discriminator(data, &DAMM_V2_POOL_DISCRIMINATOR) {
        return None;
    }
    if data.len() != o::LEN {
        return None;
    }
    Some(DammV2Pool {
        cliff_fee_numerator: read_u64(data, o::CLIFF_FEE_NUMERATOR)?,
        base_fee_mode: *data.get(o::BASE_FEE_MODE)?,
        protocol_fee_percent: *data.get(o::PROTOCOL_FEE_PERCENT)?,
        dynamic_fee_initialized: *data.get(o::DYNAMIC_FEE_INITIALIZED)? != 0,
        token_a_mint: read_pubkey(data, o::TOKEN_A_MINT)?,
        token_b_mint: read_pubkey(data, o::TOKEN_B_MINT)?,
        token_a_vault: read_pubkey(data, o::TOKEN_A_VAULT)?,
        token_b_vault: read_pubkey(data, o::TOKEN_B_VAULT)?,
        liquidity: read_u128(data, o::LIQUIDITY)?,
        sqrt_min_price: read_u128(data, o::SQRT_MIN_PRICE)?,
        sqrt_max_price: read_u128(data, o::SQRT_MAX_PRICE)?,
        sqrt_price: read_u128(data, o::SQRT_PRICE)?,
        pool_status: *data.get(o::POOL_STATUS)?,
        collect_fee_mode: *data.get(o::COLLECT_FEE_MODE)?,
        fee_version: *data.get(o::FEE_VERSION)?,
        token_a_is_token2022: *data.get(o::TOKEN_A_FLAG)? != 0,
        token_b_is_token2022: *data.get(o::TOKEN_B_FLAG)? != 0,
    })
}

// ---- Raydium CLMM (`amm::PoolState`, DEPLOYED LEGACY 1544-byte layout) ----
/// Raydium CLMM `PoolState` discriminator (`sha256("account:PoolState")[..8]`). COLLIDES with
/// Raydium CPMM `PoolState`; disambiguate by owner + the exact LEN guard (1544 vs CPMM 637).
pub const RAYDIUM_CLMM_POOL_DISCRIMINATOR: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
/// Raydium CLMM `AmmConfig` discriminator (`sha256("account:AmmConfig")[..8]`). Shares the tag
/// with Raydium CPMM `AmmConfig` but is a 117-byte account with `trade_fee_rate` at a different
/// offset — pin by owner + LEN.
pub const RAYDIUM_CLMM_AMM_CONFIG_DISCRIMINATOR: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];
/// Raydium CLMM fee denominator (`FEE_RATE_DENOMINATOR_VALUE`): `trade_fee_rate` is over 1e6.
pub const RAYDIUM_CLMM_FEE_DENOMINATOR: u64 = 1_000_000;

/// Verified legacy `PoolState` byte offsets (absolute, incl. the 8-byte discriminator).
pub mod raydium_clmm_pool_offsets {
    pub const AMM_CONFIG: usize = 9; // points to the AmmConfig holding trade_fee_rate
    pub const TOKEN_MINT_0: usize = 73;
    pub const TOKEN_MINT_1: usize = 105;
    pub const TOKEN_VAULT_0: usize = 137;
    pub const TOKEN_VAULT_1: usize = 169;
    pub const OBSERVATION_KEY: usize = 201;
    pub const MINT_DECIMALS_0: usize = 233; // u8
    pub const MINT_DECIMALS_1: usize = 234; // u8
    pub const TICK_SPACING: usize = 235; // u16
    pub const LIQUIDITY: usize = 237; // u128 (in-range L)
    pub const SQRT_PRICE_X64: usize = 253; // u128 (Q64.64)
    pub const TICK_CURRENT: usize = 269; // i32
    pub const STATUS: usize = 389; // u8 (bit4/0x10 = swap disabled)
    pub const LEN: usize = 1544;
}

/// Verified legacy `AmmConfig` byte offsets.
pub mod raydium_clmm_amm_config_offsets {
    pub const TRADE_FEE_RATE: usize = 47; // u32, over 1e6
    pub const TICK_SPACING: usize = 51; // u16
    pub const LEN: usize = 117;
}

/// Decoded Raydium CLMM legacy `PoolState` (swap/price-relevant fields). `liquidity` +
/// `sqrt_price_x64` + `tick_current` drive the in-range sqrt-price quoter; `amm_config` resolves
/// the static `trade_fee_rate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaydiumClmmPool {
    pub amm_config: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub observation_key: Pubkey,
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub status: u8,
}

impl RaydiumClmmPool {
    /// `status` bit 4 (mask `0x10`) set ⇒ swaps disabled.
    pub const STATUS_SWAP_DISABLED_BIT: u8 = 0b1_0000;

    /// True iff the pool currently permits swaps (bit 4 clear).
    pub fn swap_enabled(&self) -> bool {
        self.status & Self::STATUS_SWAP_DISABLED_BIT == 0
    }
}

/// Decode a Raydium CLMM legacy `PoolState`. `None` on wrong discriminator or wrong/short buffer
/// (the exact-LEN guard rejects a Raydium CPMM `PoolState`, which shares the discriminator).
pub fn decode_raydium_clmm_pool(data: &[u8]) -> Option<RaydiumClmmPool> {
    use raydium_clmm_pool_offsets as o;
    if !has_anchor_discriminator(data, &RAYDIUM_CLMM_POOL_DISCRIMINATOR) {
        return None;
    }
    if data.len() != o::LEN {
        return None;
    }
    Some(RaydiumClmmPool {
        amm_config: read_pubkey(data, o::AMM_CONFIG)?,
        token_mint_0: read_pubkey(data, o::TOKEN_MINT_0)?,
        token_mint_1: read_pubkey(data, o::TOKEN_MINT_1)?,
        token_vault_0: read_pubkey(data, o::TOKEN_VAULT_0)?,
        token_vault_1: read_pubkey(data, o::TOKEN_VAULT_1)?,
        observation_key: read_pubkey(data, o::OBSERVATION_KEY)?,
        mint_decimals_0: *data.get(o::MINT_DECIMALS_0)?,
        mint_decimals_1: *data.get(o::MINT_DECIMALS_1)?,
        tick_spacing: read_u16(data, o::TICK_SPACING)?,
        liquidity: read_u128(data, o::LIQUIDITY)?,
        sqrt_price_x64: read_u128(data, o::SQRT_PRICE_X64)?,
        tick_current: read_i32(data, o::TICK_CURRENT)?,
        status: *data.get(o::STATUS)?,
    })
}

/// Raydium CLMM `trade_fee_rate` (numerator over [`RAYDIUM_CLMM_FEE_DENOMINATOR`]), read from the
/// pool's separate legacy `AmmConfig` account. `None` on wrong discriminator or wrong/short buffer.
pub fn decode_raydium_clmm_trade_fee_rate(data: &[u8]) -> Option<u32> {
    if !has_anchor_discriminator(data, &RAYDIUM_CLMM_AMM_CONFIG_DISCRIMINATOR) {
        return None;
    }
    if data.len() != raydium_clmm_amm_config_offsets::LEN {
        return None;
    }
    read_u32(data, raydium_clmm_amm_config_offsets::TRADE_FEE_RATE)
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

    // ---- Fase 2.5 decoders (detection-11) ----
    //
    // We don't embed a full real-byte base64 fixture for these (would need a fresh live RPC pull
    // beyond this session), so instead we lock every verified OFFSET by writing the research's
    // live-mainnet field VALUES at their verified offsets into a correctly-sized buffer and
    // asserting the decoder reads them back. This pins the offset arithmetic + discriminator +
    // exact-LEN guard; the byte-for-byte CPI differential is the M1-GATE-EXT step.

    fn put_pk(b: &mut [u8], o: usize, p: &Pubkey) {
        b[o..o + 32].copy_from_slice(&p.to_bytes());
    }
    fn put_u16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_i32(b: &mut [u8], o: usize, v: i32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u64(b: &mut [u8], o: usize, v: u64) {
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u128(b: &mut [u8], o: usize, v: u128) {
        b[o..o + 16].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn dlmm_lb_pair_decodes_at_verified_offsets() {
        use dlmm_lb_pair_offsets as o;
        let mx = Pubkey::new_from_array([0x11; 32]);
        let my = Pubkey::new_from_array([0x22; 32]);
        let rx = Pubkey::new_from_array([0x33; 32]);
        let ry = Pubkey::new_from_array([0x44; 32]);
        let mut b = vec![0u8; o::LEN];
        b[0..8].copy_from_slice(&DLMM_LB_PAIR_DISCRIMINATOR);
        // Live SOL/USDC LbPair values (research snapshot).
        put_u16(&mut b, o::BASE_FACTOR, 10_000);
        put_u16(&mut b, o::PROTOCOL_SHARE, 1_000);
        b[o::BASE_FEE_POWER_FACTOR] = 0;
        b[o::COLLECT_FEE_MODE] = 0; // InputOnly
        put_i32(&mut b, o::ACTIVE_ID, -26_387);
        put_u16(&mut b, o::BIN_STEP, 1);
        b[o::STATUS] = 0;
        put_pk(&mut b, o::TOKEN_X_MINT, &mx);
        put_pk(&mut b, o::TOKEN_Y_MINT, &my);
        put_pk(&mut b, o::RESERVE_X, &rx);
        put_pk(&mut b, o::RESERVE_Y, &ry);
        b[o::TOKEN_MINT_X_PROGRAM_FLAG] = 0;
        b[o::TOKEN_MINT_Y_PROGRAM_FLAG] = 0;

        let p = decode_dlmm_lb_pair(&b).expect("dlmm decodes");
        assert_eq!(p.active_id, -26_387);
        assert_eq!(p.bin_step, 1);
        assert_eq!(p.base_factor, 10_000);
        assert_eq!(p.protocol_share, 1_000);
        assert_eq!(p.collect_fee_mode, 0);
        assert!(p.swap_enabled());
        assert_eq!(p.token_x_mint, mx);
        assert_eq!(p.token_y_mint, my);
        assert_eq!(p.reserve_x, rx);
        assert_eq!(p.reserve_y, ry);
        assert!(!p.token_x_is_token2022 && !p.token_y_is_token2022);
        // base_fee_rate = 10000 * 1 * 10 * 10^0 = 100_000 over 1e9 = 0.01%.
        assert_eq!(p.base_fee_rate(), Some(100_000));
        assert_eq!(DLMM_FEE_DENOMINATOR, 1_000_000_000);

        // Wrong LEN (e.g. truncated) and wrong discriminator both fail closed.
        assert!(decode_dlmm_lb_pair(&b[..o::LEN - 1]).is_none());
        let mut bad = b.clone();
        bad[0] ^= 0xFF;
        assert!(decode_dlmm_lb_pair(&bad).is_none());
    }

    #[test]
    fn damm_v2_pool_decodes_at_verified_offsets() {
        use damm_v2_pool_offsets as o;
        let am = Pubkey::new_from_array([0xA1; 32]);
        let bm = Pubkey::new_from_array([0xB2; 32]);
        let av = Pubkey::new_from_array([0xA3; 32]);
        let bv = Pubkey::new_from_array([0xB4; 32]);
        let mut b = vec![0u8; o::LEN];
        b[0..8].copy_from_slice(&DAMM_V2_POOL_DISCRIMINATOR);
        // Verified mainnet pool E8zRkDw… fee/band fields.
        put_u64(&mut b, o::CLIFF_FEE_NUMERATOR, 500_000_000);
        b[o::BASE_FEE_MODE] = 1; // TimeExponential
        b[o::PROTOCOL_FEE_PERCENT] = 20;
        b[o::DYNAMIC_FEE_INITIALIZED] = 1;
        put_pk(&mut b, o::TOKEN_A_MINT, &am);
        put_pk(&mut b, o::TOKEN_B_MINT, &bm);
        put_pk(&mut b, o::TOKEN_A_VAULT, &av);
        put_pk(&mut b, o::TOKEN_B_VAULT, &bv);
        put_u128(&mut b, o::LIQUIDITY, 123_456_789_000);
        put_u128(&mut b, o::SQRT_MIN_PRICE, 4_295_048_016);
        put_u128(
            &mut b,
            o::SQRT_MAX_PRICE,
            79_226_673_521_066_979_257_578_248_091,
        );
        put_u128(&mut b, o::SQRT_PRICE, 1u128 << 64);
        b[o::POOL_STATUS] = 0;
        b[o::COLLECT_FEE_MODE] = 1; // OnlyB
        b[o::FEE_VERSION] = 0;
        b[o::TOKEN_A_FLAG] = 0;
        b[o::TOKEN_B_FLAG] = 0;

        let p = decode_damm_v2_pool(&b).expect("damm v2 decodes");
        assert_eq!(p.cliff_fee_numerator, 500_000_000);
        assert_eq!(p.base_fee_mode, 1);
        assert_eq!(p.protocol_fee_percent, 20);
        assert!(p.dynamic_fee_initialized);
        assert_eq!(p.liquidity, 123_456_789_000);
        assert_eq!(p.sqrt_min_price, 4_295_048_016);
        assert_eq!(p.sqrt_max_price, 79_226_673_521_066_979_257_578_248_091);
        assert_eq!(p.sqrt_price, 1u128 << 64);
        assert_eq!(p.collect_fee_mode, 1);
        assert_eq!(p.max_fee_numerator(), 500_000_000); // fee_version 0
        assert!(p.swap_enabled());
        assert_eq!(p.token_a_mint, am);
        assert_eq!(p.token_b_vault, bv);

        // A PumpSwap `Pool` shares the discriminator but is NOT 1112 bytes → rejected.
        let mut pumpswap_len = vec![0u8; 300];
        pumpswap_len[0..8].copy_from_slice(&DAMM_V2_POOL_DISCRIMINATOR);
        assert!(decode_damm_v2_pool(&pumpswap_len).is_none());
    }

    #[test]
    fn raydium_clmm_pool_and_config_decode_at_verified_offsets() {
        use raydium_clmm_pool_offsets as o;
        let cfg = Pubkey::new_from_array([0xC1; 32]);
        let m0 = Pubkey::new_from_array([0x01; 32]);
        let m1 = Pubkey::new_from_array([0x02; 32]);
        let v0 = Pubkey::new_from_array([0x03; 32]);
        let v1 = Pubkey::new_from_array([0x04; 32]);
        let obs = Pubkey::new_from_array([0x05; 32]);
        let mut b = vec![0u8; o::LEN];
        b[0..8].copy_from_slice(&RAYDIUM_CLMM_POOL_DISCRIMINATOR);
        // Live SOL/USDC pool 3ucNos4… values.
        put_pk(&mut b, o::AMM_CONFIG, &cfg);
        put_pk(&mut b, o::TOKEN_MINT_0, &m0);
        put_pk(&mut b, o::TOKEN_MINT_1, &m1);
        put_pk(&mut b, o::TOKEN_VAULT_0, &v0);
        put_pk(&mut b, o::TOKEN_VAULT_1, &v1);
        put_pk(&mut b, o::OBSERVATION_KEY, &obs);
        b[o::MINT_DECIMALS_0] = 9;
        b[o::MINT_DECIMALS_1] = 6;
        put_u16(&mut b, o::TICK_SPACING, 1);
        put_u128(&mut b, o::LIQUIDITY, 92_625_898_297_868);
        put_u128(&mut b, o::SQRT_PRICE_X64, 4_930_817_903_949_873_103);
        put_i32(&mut b, o::TICK_CURRENT, -26_389);
        b[o::STATUS] = 0;

        let p = decode_raydium_clmm_pool(&b).expect("clmm pool decodes");
        assert_eq!(p.amm_config, cfg);
        assert_eq!(p.tick_spacing, 1);
        assert_eq!(p.liquidity, 92_625_898_297_868);
        assert_eq!(p.sqrt_price_x64, 4_930_817_903_949_873_103);
        assert_eq!(p.tick_current, -26_389);
        assert_eq!(p.mint_decimals_0, 9);
        assert_eq!(p.mint_decimals_1, 6);
        assert!(p.swap_enabled());
        // sqrt_price decodes to a sane SOL/USDC level after decimal adjust (9 base / 6 quote).
        let usdc_per_sol = sqrt_price_x64_to_price(p.sqrt_price_x64) * 1_000.0;
        assert!((20.0..2_000.0).contains(&usdc_per_sol), "{usdc_per_sol}");

        // status bit 4 disables swaps.
        let mut disabled = b.clone();
        disabled[o::STATUS] = RaydiumClmmPool::STATUS_SWAP_DISABLED_BIT;
        assert!(!decode_raydium_clmm_pool(&disabled).unwrap().swap_enabled());

        // A Raydium CPMM `PoolState` shares the discriminator but is 637 bytes → rejected.
        let mut cpmm_len = vec![0u8; offsets::raydium_cpmm_pool::LEN];
        cpmm_len[0..8].copy_from_slice(&RAYDIUM_CLMM_POOL_DISCRIMINATOR);
        assert!(decode_raydium_clmm_pool(&cpmm_len).is_none());

        // AmmConfig: trade_fee_rate=400 (0.04%) at offset 47.
        let mut cfgbuf = vec![0u8; raydium_clmm_amm_config_offsets::LEN];
        cfgbuf[0..8].copy_from_slice(&RAYDIUM_CLMM_AMM_CONFIG_DISCRIMINATOR);
        put_u32(
            &mut cfgbuf,
            raydium_clmm_amm_config_offsets::TRADE_FEE_RATE,
            400,
        );
        assert_eq!(decode_raydium_clmm_trade_fee_rate(&cfgbuf), Some(400));
        assert_eq!(RAYDIUM_CLMM_FEE_DENOMINATOR, 1_000_000);
        assert!(decode_raydium_clmm_trade_fee_rate(&cfgbuf[..10]).is_none());
    }
}
