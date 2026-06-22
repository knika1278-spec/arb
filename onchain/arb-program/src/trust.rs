//! Trust boundary (invariant §6, the load-bearing on-chain security gate). Pool accounts
//! arrive **untrusted** via `remaining_accounts`, so before the program reads a balance or
//! issues a swap CPI it verifies:
//!   (a) every swap-CPI target is an allowlisted Wave-1 DEX program id, and
//!   (b) the token accounts whose balance gates the profit-assert are owned by the bot
//!       authority — a griefer cannot substitute an account that fakes a profit.

use crate::allowlist::is_allowlisted_dex;
use crate::error::to_program_error;
use crate::state::read_token_owner;
use arb_types::ArbError;
use solana_program::account_info::AccountInfo;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

/// (a) Reject a swap-CPI whose target program is not allowlisted.
pub fn verify_swap_program(program_id: &Pubkey) -> Result<(), ProgramError> {
    if is_allowlisted_dex(program_id) {
        Ok(())
    } else {
        Err(to_program_error(ArbError::UnauthorizedProgram))
    }
}

/// (b) Reject a balance-read account not owned by the bot authority.
pub fn verify_balance_account_owner(
    token_account: &AccountInfo,
    authority: &Pubkey,
) -> Result<(), ProgramError> {
    let owner = read_token_owner(token_account)?;
    if owner == *authority {
        Ok(())
    } else {
        Err(to_program_error(ArbError::UnauthorizedTokenAccountOwner))
    }
}

/// The authority must have actually signed the transaction.
pub fn verify_authority_signer(authority: &AccountInfo) -> Result<(), ProgramError> {
    if authority.is_signer {
        Ok(())
    } else {
        Err(to_program_error(ArbError::MissingRequiredSignature))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlisted_program_passes_others_rejected() {
        assert!(verify_swap_program(&arb_config::WAVE1_DEX_ALLOWLIST[0]).is_ok());
        let bad = Pubkey::new_from_array([3u8; 32]);
        assert_eq!(
            verify_swap_program(&bad),
            Err(ProgramError::Custom(ArbError::UnauthorizedProgram.code()))
        );
    }
}
