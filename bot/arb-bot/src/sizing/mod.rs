//! Sizing layer over `arb-math` (the bot-facing side of sizing-3..8). Takes a base asset +
//! two pools of the same pair, orients the two legs into a round-trip that returns to the
//! base, and sizes it at the 90–95% policy. Dependency-free of `detection` (the contract is
//! one-way: detection → sizing), so the caller maps `PoolQuote → PoolRef`.

use arb_config::program_ids::{
    METEORA_DAMM_V2, METEORA_DLMM, ORCA_WHIRLPOOL, PUMPSWAP_AMM, RAYDIUM_CLMM, RAYDIUM_CPMM,
};
use arb_math::{size_round_trip, CpmmReserves, RoundTrip, SizingPolicy};
use arb_types::{DexKind, SwapDir};
use solana_pubkey::Pubkey;

/// sizing-3 venue registry: the on-chain swap program id for a venue, byte-equal to the pinned
/// `arb-config` constants. The registry MIRRORS (never redefines) the on-chain trust boundary, so
/// a quoted venue and the program the tx-builder is allowed to invoke can never drift apart.
/// Wave-1 venues resolve into [`arb_config::is_allowlisted_swap_program`]; the Fase-2.5 venues
/// (tags 3–5) resolve into the gated [`arb_config::is_fase25_swap_program`] set instead.
pub fn venue_program_id(dex: DexKind) -> Pubkey {
    match dex {
        DexKind::RaydiumCpmm => RAYDIUM_CPMM,
        DexKind::OrcaWhirlpool => ORCA_WHIRLPOOL,
        DexKind::PumpSwapAmm => PUMPSWAP_AMM,
        DexKind::MeteoraDlmm => METEORA_DLMM,
        DexKind::MeteoraDammV2 => METEORA_DAMM_V2,
        DexKind::RaydiumClmm => RAYDIUM_CLMM,
    }
}

/// A pool as sizing needs it: reserves (oriented `reserve_a`↔`mint_a`) + its two mints.
#[derive(Clone, Copy, Debug)]
pub struct PoolRef {
    pub dex: DexKind,
    pub reserves: CpmmReserves,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
}

impl PoolRef {
    /// Swap direction whose INPUT side is `input_mint`, or `None` if the mint isn't in the pool.
    fn dir_with_input(&self, input_mint: Pubkey) -> Option<SwapDir> {
        if input_mint == self.mint_a {
            Some(SwapDir::AtoB)
        } else if input_mint == self.mint_b {
            Some(SwapDir::BtoA)
        } else {
            None
        }
    }

    /// The mint on the OTHER side from `input_mint`.
    fn other_mint(&self, input_mint: Pubkey) -> Option<Pubkey> {
        if input_mint == self.mint_a {
            Some(self.mint_b)
        } else if input_mint == self.mint_b {
            Some(self.mint_a)
        } else {
            None
        }
    }
}

/// A fully-sized, orientation-resolved round-trip ready for the tx-builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizedTrade {
    pub base_mint: Pubkey,
    pub intermediate_mint: Pubkey,
    pub dir_a: SwapDir,
    pub dir_b: SwapDir,
    pub dex_a: DexKind,
    pub dex_b: DexKind,
    pub size_in: u64,
    /// Predicted intermediate out of leg A (bit-exact mirror of on-chain).
    pub predicted_mid: u64,
    /// Predicted final base out of leg B.
    pub predicted_out: u64,
    /// Per-leg minimums (tolerance-adjusted) for the on-chain slippage guards.
    pub min_out_a: u64,
    pub min_out_b: u64,
    pub expected_profit: i128,
}

/// Build the round-trip `base → (pool_a) → intermediate → (pool_b) → base`. Returns the
/// `RoundTrip` plus the resolved directions + intermediate mint, or `None` if the two pools
/// do not share the base + a common intermediate.
fn build_round_trip(
    base: Pubkey,
    pool_a: &PoolRef,
    pool_b: &PoolRef,
) -> Option<(RoundTrip, SwapDir, SwapDir, Pubkey)> {
    let dir_a = pool_a.dir_with_input(base)?;
    let intermediate = pool_a.other_mint(base)?;
    // Leg B must take the intermediate as input and return the base.
    let dir_b = pool_b.dir_with_input(intermediate)?;
    if pool_b.other_mint(intermediate)? != base {
        return None;
    }
    let rt = RoundTrip::new(pool_a.reserves, dir_a, pool_b.reserves, dir_b);
    Some((rt, dir_a, dir_b, intermediate))
}

/// Size a round-trip across two pools for a given base asset. `tolerance_bps` slackens the
/// per-leg minimums below the bit-exact prediction (0 = strict; rely on the M1-GATE).
pub fn size_two_pool(
    base: Pubkey,
    pool_a: &PoolRef,
    pool_b: &PoolRef,
    policy: SizingPolicy,
    tolerance_bps: u32,
) -> Option<SizedTrade> {
    let (rt, dir_a, dir_b, intermediate) = build_round_trip(base, pool_a, pool_b)?;
    let (size_in, predicted_out, expected_profit) = size_round_trip(&rt, policy)?;
    let predicted_mid = rt.pool_a.quote_out(dir_a, size_in)?;

    let slacken = |v: u64| -> u64 {
        let bps = 10_000u128.saturating_sub(tolerance_bps.min(10_000) as u128);
        ((v as u128).saturating_mul(bps) / 10_000) as u64
    };

    Some(SizedTrade {
        base_mint: base,
        intermediate_mint: intermediate,
        dir_a,
        dir_b,
        dex_a: pool_a.dex,
        dex_b: pool_b.dex,
        size_in,
        predicted_mid,
        predicted_out,
        min_out_a: slacken(predicted_mid),
        min_out_b: slacken(predicted_out),
        expected_profit,
    })
}

/// Try both pool orderings (which pool is leg A) and return the more profitable sized trade.
pub fn size_best_direction(
    base: Pubkey,
    p1: &PoolRef,
    p2: &PoolRef,
    policy: SizingPolicy,
    tolerance_bps: u32,
) -> Option<SizedTrade> {
    let a = size_two_pool(base, p1, p2, policy, tolerance_bps);
    let b = size_two_pool(base, p2, p1, policy, tolerance_bps);
    match (a, b) {
        (Some(x), Some(y)) => Some(if x.expected_profit >= y.expected_profit {
            x
        } else {
            y
        }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn pool(ra: u64, rb: u64, ma: u8, mb: u8) -> PoolRef {
        PoolRef {
            dex: DexKind::RaydiumCpmm,
            reserves: CpmmReserves::new(ra, rb, 25, 10_000),
            mint_a: mint(ma),
            mint_b: mint(mb),
        }
    }

    #[test]
    fn sizes_profitable_round_trip() {
        // base = mint 1. Pool A: 1M base / 2M intermediate ; Pool B: 2M intermediate / 1.1M base.
        let base = mint(1);
        let a = pool(1_000_000, 2_000_000, 1, 2);
        let b = pool(2_000_000, 1_100_000, 2, 1);
        let st = size_best_direction(base, &a, &b, SizingPolicy::DEFAULT, 50).expect("profit");
        assert!(st.expected_profit > 0);
        assert_eq!(st.base_mint, base);
        assert_eq!(st.intermediate_mint, mint(2));
        assert_eq!(st.dir_a, SwapDir::AtoB); // input base (mint_a) on pool A
        assert!(st.min_out_a <= st.predicted_mid && st.min_out_b <= st.predicted_out);
        assert_eq!(
            st.predicted_out as i128 - st.size_in as i128,
            st.expected_profit
        );
    }

    #[test]
    fn no_trade_when_pools_dont_share_intermediate() {
        let base = mint(1);
        let a = pool(1_000_000, 2_000_000, 1, 2);
        let c = pool(1_000_000, 1_000_000, 1, 3); // shares base but intermediate is mint 3, not 2
        assert!(size_two_pool(base, &a, &c, SizingPolicy::DEFAULT, 0).is_none());
    }

    #[test]
    fn no_trade_when_balanced() {
        let base = mint(1);
        let a = pool(1_000_000, 1_000_000, 1, 2);
        let b = pool(1_000_000, 1_000_000, 2, 1);
        assert!(size_best_direction(base, &a, &b, SizingPolicy::DEFAULT, 0).is_none());
    }

    #[test]
    fn venue_program_ids_mirror_the_onchain_allowlist() {
        use arb_config::program_ids::is_allowlisted_swap_program;
        for (dex, pid) in [
            (DexKind::RaydiumCpmm, RAYDIUM_CPMM),
            (DexKind::OrcaWhirlpool, ORCA_WHIRLPOOL),
            (DexKind::PumpSwapAmm, PUMPSWAP_AMM),
        ] {
            // The registry returns the byte-identical pinned arb-config constant...
            assert_eq!(venue_program_id(dex), pid);
            // ...and every venue id is in the on-chain swap allowlist.
            assert!(is_allowlisted_swap_program(&pid), "{dex:?} not allowlisted");
        }
    }
}
