//! add-6 — Whirlpool tick-array / oracle on-demand PDA resolver.
//!
//! Orca Whirlpool tick arrays and the oracle are PDAs, not subscribed accounts (plan §5: +0
//! subscriptions, fetched on-demand). The in-range `swap_v2` CPI needs the current tick array plus
//! the next two in the swap direction, and the oracle PDA — resolved consistently for BOTH the
//! off-chain quote and the on-chain `remaining_accounts`. Multi-array crossing (a swap that exhausts
//! the three provided arrays) is out of scope for M1: the sizing quoter returns
//! [`WhirlpoolResolveError::CrossesTick`] (a Fase-3 case) rather than guessing.
//!
//! Derivations (Orca canonical):
//! * `TICK_ARRAY_SIZE = 88`; a tick array spans `tick_spacing · 88` ticks.
//! * tick-array PDA seeds = `["tick_array", whirlpool, start_tick_index_decimal_string]`.
//! * oracle PDA seeds = `["oracle", whirlpool]`.

use arb_config::program_ids::ORCA_WHIRLPOOL;
use solana_pubkey::Pubkey;

/// Number of ticks per tick array (Orca constant).
pub const TICK_ARRAY_SIZE: i32 = 88;

/// Why a Whirlpool route could not be resolved for M1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhirlpoolResolveError {
    /// `tick_spacing` was zero (degenerate pool).
    ZeroTickSpacing,
    /// The swap would cross beyond the three resolved tick arrays — Fase 3.
    CrossesTick,
}

/// The accounts a single-range `swap_v2` CPI consumes beyond the pool/vaults: three tick arrays in
/// the swap direction (index 0 = current) + the oracle PDA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WhirlpoolSwapAccounts {
    pub tick_array_0: Pubkey,
    pub tick_array_1: Pubkey,
    pub tick_array_2: Pubkey,
    pub oracle: Pubkey,
}

/// The number of ticks spanned by one tick array at `tick_spacing`.
fn ticks_per_array(tick_spacing: u16) -> i32 {
    tick_spacing as i32 * TICK_ARRAY_SIZE
}

/// The start tick index of the array containing `tick_current` (floored toward −∞ so negative ticks
/// land in the correct array — `div_euclid` gives floor division for a positive divisor).
pub fn start_tick_index(
    tick_current: i32,
    tick_spacing: u16,
) -> Result<i32, WhirlpoolResolveError> {
    if tick_spacing == 0 {
        return Err(WhirlpoolResolveError::ZeroTickSpacing);
    }
    let span = ticks_per_array(tick_spacing);
    Ok(tick_current.div_euclid(span) * span)
}

/// Derive a tick-array PDA from its start index (Orca uses the decimal string of the start index).
pub fn tick_array_pda(whirlpool: &Pubkey, start_index: i32) -> Pubkey {
    let s = start_index.to_string();
    Pubkey::find_program_address(
        &[b"tick_array", whirlpool.as_ref(), s.as_bytes()],
        &ORCA_WHIRLPOOL,
    )
    .0
}

/// Derive the Whirlpool oracle PDA.
pub fn oracle_pda(whirlpool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"oracle", whirlpool.as_ref()], &ORCA_WHIRLPOOL).0
}

/// Resolve the three swap-direction tick arrays + oracle for an in-range `swap_v2`. `a_to_b` is the
/// zero-for-one direction (price/tick DECREASING); `b_to_a` increases the tick, so the next arrays
/// step to higher start indices.
pub fn resolve_swap_accounts(
    whirlpool: &Pubkey,
    tick_current: i32,
    tick_spacing: u16,
    a_to_b: bool,
) -> Result<WhirlpoolSwapAccounts, WhirlpoolResolveError> {
    let span = ticks_per_array(tick_spacing);
    if span == 0 {
        return Err(WhirlpoolResolveError::ZeroTickSpacing);
    }
    let start0 = start_tick_index(tick_current, tick_spacing)?;
    // a_to_b walks DOWN in tick index; b_to_a walks UP.
    let step = if a_to_b { -span } else { span };
    Ok(WhirlpoolSwapAccounts {
        tick_array_0: tick_array_pda(whirlpool, start0),
        tick_array_1: tick_array_pda(whirlpool, start0 + step),
        tick_array_2: tick_array_pda(whirlpool, start0 + 2 * step),
        oracle: oracle_pda(whirlpool),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Pubkey {
        Pubkey::new_from_array([7; 32])
    }

    #[test]
    fn start_index_floors_toward_negative_infinity() {
        // tick_spacing 64 => span 64*88 = 5632.
        assert_eq!(start_tick_index(0, 64).unwrap(), 0);
        assert_eq!(start_tick_index(5_631, 64).unwrap(), 0);
        assert_eq!(start_tick_index(5_632, 64).unwrap(), 5_632);
        // Negative tick: -1 must land in [-5632, 0), i.e. start -5632 (NOT 0 from truncation).
        assert_eq!(start_tick_index(-1, 64).unwrap(), -5_632);
        assert_eq!(start_tick_index(-5_632, 64).unwrap(), -5_632);
        assert_eq!(start_tick_index(-5_633, 64).unwrap(), -11_264);
    }

    #[test]
    fn zero_tick_spacing_is_rejected() {
        assert_eq!(
            start_tick_index(0, 0),
            Err(WhirlpoolResolveError::ZeroTickSpacing)
        );
        assert_eq!(
            resolve_swap_accounts(&pool(), 0, 0, true),
            Err(WhirlpoolResolveError::ZeroTickSpacing)
        );
    }

    #[test]
    fn pdas_are_deterministic_and_distinct() {
        let p = pool();
        let a = tick_array_pda(&p, 0);
        assert_eq!(a, tick_array_pda(&p, 0)); // deterministic
        assert_ne!(a, tick_array_pda(&p, 5_632)); // distinct start => distinct PDA
        assert_ne!(a, oracle_pda(&p)); // tick array != oracle
    }

    #[test]
    fn a_to_b_walks_down_b_to_a_walks_up() {
        let p = pool();
        let down = resolve_swap_accounts(&p, 10_000, 64, true).unwrap();
        let up = resolve_swap_accounts(&p, 10_000, 64, false).unwrap();
        // start0 = floor(10000/5632)*5632 = 5632.
        assert_eq!(down.tick_array_0, tick_array_pda(&p, 5_632));
        assert_eq!(down.tick_array_1, tick_array_pda(&p, 0)); // 5632 - 5632
        assert_eq!(down.tick_array_2, tick_array_pda(&p, -5_632)); // 5632 - 2*5632
        assert_eq!(up.tick_array_0, tick_array_pda(&p, 5_632));
        assert_eq!(up.tick_array_1, tick_array_pda(&p, 11_264)); // 5632 + 5632
        assert_eq!(up.tick_array_2, tick_array_pda(&p, 16_896));
        // Same oracle regardless of direction.
        assert_eq!(down.oracle, up.oracle);
    }
}
