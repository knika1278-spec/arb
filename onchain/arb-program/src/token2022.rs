//! Token-2022 mint vetting (invariant §8). HARD-REJECT any mint that can silently break a
//! leg or the profit-assert; allow only plain SPL + **fee-only** Token-2022. The check is a
//! guarded TLV scan over the raw mint bytes (no `spl-token-2022` dependency on the hot
//! path): mint base is 82 bytes; for a Token-2022 mint the extension TLV region begins after
//! the account-type byte at offset 165, each entry `type:u16 LE, len:u16 LE, value[len]`.
//!
//! REJECT: MintCloseAuthority, ConfidentialTransfer(Mint/FeeConfig), DefaultAccountState,
//! NonTransferable, PermanentDelegate, and TransferHook with a non-null program id.
//! ALLOW: TransferFeeConfig (fee-only), InterestBearing/ScaledUiAmount (display-only — raw
//! on-chain amount unchanged), and a TransferHook whose program id is null (e.g. PYUSD).

use crate::error::to_program_error;
use arb_types::ArbError;
use solana_program::account_info::AccountInfo;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

// ExtensionType discriminants (mint-relevant subset).
const EXT_TRANSFER_FEE_CONFIG: u16 = 1;
const EXT_MINT_CLOSE_AUTHORITY: u16 = 3;
const EXT_CONFIDENTIAL_TRANSFER_MINT: u16 = 4;
const EXT_DEFAULT_ACCOUNT_STATE: u16 = 6;
const EXT_NON_TRANSFERABLE: u16 = 9;
const EXT_INTEREST_BEARING: u16 = 10;
const EXT_PERMANENT_DELEGATE: u16 = 12;
const EXT_TRANSFER_HOOK: u16 = 14;
const EXT_CONFIDENTIAL_TRANSFER_FEE_CONFIG: u16 = 16;
const EXT_SCALED_UI_AMOUNT: u16 = 19;

const MINT_BASE_LEN: usize = 82;
/// Token-2022 stores the account-type byte at 165; mint TLV entries start at 166.
const TLV_START: usize = 166;

/// Vet a mint account by reading its owner program + raw data.
pub fn vet_mint(mint: &AccountInfo) -> Result<(), ProgramError> {
    let data = mint.try_borrow_data()?;
    vet_mint_bytes(mint.owner, &data)
}

/// Pure form (host-testable): `owner` is the program that owns the mint account.
pub fn vet_mint_bytes(owner: &Pubkey, data: &[u8]) -> Result<(), ProgramError> {
    let token = &arb_config::program_ids::TOKEN_PROGRAM;
    let token2022 = &arb_config::program_ids::TOKEN_2022_PROGRAM;

    if owner == token {
        // Classic SPL mint cannot carry extensions — always OK (assuming base layout).
        return Ok(());
    }
    if owner != token2022 {
        // Not a recognized token program owner for a mint.
        return Err(to_program_error(ArbError::ForbiddenTokenExtension));
    }
    // Token-2022. A bare mint with no extensions is <= 165 bytes -> OK.
    if data.len() <= TLV_START.saturating_sub(1) {
        return Ok(());
    }
    scan_tlv(&data[TLV_START..])
}

fn scan_tlv(mut tlv: &[u8]) -> Result<(), ProgramError> {
    let reject = || to_program_error(ArbError::ForbiddenTokenExtension);
    while tlv.len() >= 4 {
        let ext_type = u16::from_le_bytes([tlv[0], tlv[1]]);
        let len = u16::from_le_bytes([tlv[2], tlv[3]]) as usize;
        let value = tlv
            .get(4..4usize.checked_add(len).ok_or_else(reject)?)
            .ok_or_else(reject)?;

        match ext_type {
            // Allowed.
            EXT_TRANSFER_FEE_CONFIG | EXT_INTEREST_BEARING | EXT_SCALED_UI_AMOUNT => {}
            // Allowed only if the hook program id (value bytes 32..64) is null.
            EXT_TRANSFER_HOOK => {
                let pid = value.get(32..64).ok_or_else(reject)?;
                if pid.iter().any(|&b| b != 0) {
                    return Err(reject());
                }
            }
            // Hard-reject.
            EXT_MINT_CLOSE_AUTHORITY
            | EXT_CONFIDENTIAL_TRANSFER_MINT
            | EXT_CONFIDENTIAL_TRANSFER_FEE_CONFIG
            | EXT_DEFAULT_ACCOUNT_STATE
            | EXT_NON_TRANSFERABLE
            | EXT_PERMANENT_DELEGATE => return Err(reject()),
            // Unknown extension: reject conservatively for Milestone 1.
            _ => return Err(reject()),
        }

        let advance = 4usize.checked_add(len).ok_or_else(reject)?;
        tlv = &tlv[advance..];
    }
    Ok(())
}

/// Helper used by the off-chain mirror + tests: the base mint length.
pub const fn mint_base_len() -> usize {
    MINT_BASE_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t2022() -> Pubkey {
        arb_config::program_ids::TOKEN_2022_PROGRAM
    }

    fn mint_with_tlv(entries: &[(u16, &[u8])]) -> Vec<u8> {
        let mut v = vec![0u8; TLV_START];
        for (ty, val) in entries {
            v.extend_from_slice(&ty.to_le_bytes());
            v.extend_from_slice(&(val.len() as u16).to_le_bytes());
            v.extend_from_slice(val);
        }
        v
    }

    #[test]
    fn plain_spl_mint_ok() {
        let spl = arb_config::program_ids::TOKEN_PROGRAM;
        assert!(vet_mint_bytes(&spl, &[0u8; MINT_BASE_LEN]).is_ok());
    }

    #[test]
    fn fee_only_token2022_ok() {
        let data = mint_with_tlv(&[(EXT_TRANSFER_FEE_CONFIG, &[0u8; 108])]);
        assert!(vet_mint_bytes(&t2022(), &data).is_ok());
    }

    #[test]
    fn bare_token2022_mint_ok() {
        assert!(vet_mint_bytes(&t2022(), &[0u8; 165]).is_ok());
    }

    #[test]
    fn rejects_forbidden_extensions() {
        for ty in [
            EXT_MINT_CLOSE_AUTHORITY,
            EXT_NON_TRANSFERABLE,
            EXT_PERMANENT_DELEGATE,
            EXT_DEFAULT_ACCOUNT_STATE,
            EXT_CONFIDENTIAL_TRANSFER_MINT,
        ] {
            let data = mint_with_tlv(&[(ty, &[0u8; 32])]);
            assert!(
                vet_mint_bytes(&t2022(), &data).is_err(),
                "ext {ty} should reject"
            );
        }
    }

    #[test]
    fn transfer_hook_null_ok_nonnull_rejected() {
        // value: authority(32) + program_id(32). Null program id -> OK.
        let ok = mint_with_tlv(&[(EXT_TRANSFER_HOOK, &[0u8; 64])]);
        assert!(vet_mint_bytes(&t2022(), &ok).is_ok());
        // Non-null program id -> reject.
        let mut val = [0u8; 64];
        val[32] = 1;
        let bad = mint_with_tlv(&[(EXT_TRANSFER_HOOK, &val)]);
        assert!(vet_mint_bytes(&t2022(), &bad).is_err());
    }
}
