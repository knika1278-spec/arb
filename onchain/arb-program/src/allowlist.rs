//! Authoritative on-chain DEX allowlist. Delegates to the SHARED `arb_config` const table
//! (single source of truth, invariant §6) — `solana_program::pubkey::Pubkey` is the same
//! type as `solana_pubkey::Pubkey` in Agave 2.x, so no conversion is needed.

use solana_program::pubkey::Pubkey;

/// Trust-boundary membership test: is `program_id` an allowlisted Wave-1 swap program?
#[inline]
pub fn is_allowlisted_dex(program_id: &Pubkey) -> bool {
    arb_config::is_allowlisted_swap_program(program_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegates_to_shared_allowlist() {
        // The shared const table is the source; just prove linkage + a negative case.
        assert!(!is_allowlisted_dex(&Pubkey::new_from_array([0u8; 32])));
        assert_eq!(arb_config::WAVE1_DEX_ALLOWLIST.len(), 3);
        assert!(is_allowlisted_dex(&arb_config::WAVE1_DEX_ALLOWLIST[0]));
    }
}
