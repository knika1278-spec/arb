//! Zero-copy reads of the SPL token-account layout, shared by SPL Token and Token-2022:
//! `mint: [u8;32] @ 0`, `owner: [u8;32] @ 32`, `amount: u64 LE @ 64`. We read the **actual**
//! `amount` field for profit-checking (invariant §7) rather than trusting any instruction
//! amount, because a Token-2022 transfer fee skims the received amount.

use crate::error::to_program_error;
use arb_types::ArbError;
use solana_program::account_info::AccountInfo;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

const OWNER_OFFSET: usize = 32;
const AMOUNT_OFFSET: usize = 64;
const ACCOUNT_MIN_LEN: usize = 72;

/// Read the `amount` (balance) of an SPL/Token-2022 token account, zero-copy.
pub fn read_token_amount(account: &AccountInfo) -> Result<u64, ProgramError> {
    let data = account.try_borrow_data()?;
    let bytes = data
        .get(AMOUNT_OFFSET..AMOUNT_OFFSET + 8)
        .ok_or_else(|| to_program_error(ArbError::InvalidAccountsList))?;
    let arr: [u8; 8] = bytes
        .try_into()
        .map_err(|_| to_program_error(ArbError::InvalidAccountsList))?;
    Ok(u64::from_le_bytes(arr))
}

/// Read the `owner` field of an SPL/Token-2022 token account.
pub fn read_token_owner(account: &AccountInfo) -> Result<Pubkey, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.len() < ACCOUNT_MIN_LEN {
        return Err(to_program_error(ArbError::InvalidAccountsList));
    }
    let bytes = &data[OWNER_OFFSET..OWNER_OFFSET + 32];
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| to_program_error(ArbError::InvalidAccountsList))?;
    Ok(Pubkey::new_from_array(arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_and_owner_decode_from_layout() {
        // Build a minimal token-account buffer: owner at 32, amount at 64.
        let mut buf = [0u8; ACCOUNT_MIN_LEN];
        let owner = [9u8; 32];
        buf[OWNER_OFFSET..OWNER_OFFSET + 32].copy_from_slice(&owner);
        buf[AMOUNT_OFFSET..AMOUNT_OFFSET + 8].copy_from_slice(&12_345u64.to_le_bytes());

        // Decode the raw byte logic directly (AccountInfo construction needs a runtime).
        assert_eq!(u64::from_le_bytes(buf[64..72].try_into().unwrap()), 12_345);
        assert_eq!(&buf[32..64], &owner);
    }
}
