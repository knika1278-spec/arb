//! Token-2022 HARD-REJECT filter (txbuilder-3) — the off-chain *mirror* of the on-chain
//! `arb_program::token2022::vet_mint`.
//!
//! The strongest possible mirror is not a re-implementation but a direct call into the SAME
//! function the on-chain program runs (we link `arb-program` no-entrypoint). This makes drift
//! between "what the tx-builder vets" and "what the program enforces" structurally impossible
//! (invariant §8). We only translate the program's `ProgramError` into a tx-builder error
//! that carries the offending mint for diagnostics.

use crate::txbuilder::error::TxBuilderError;
use solana_pubkey::Pubkey;

/// Vet a single mint by its owner program + raw account bytes. `mint` is the mint pubkey,
/// carried only so a rejection can name it.
///
/// ALLOW: plain SPL, and fee-only Token-2022 (TransferFee / interest-bearing / scaled-ui /
/// null-program TransferHook). REJECT: hook(non-null) / non-transferable / permanent-delegate
/// / confidential / default-account-state / mint-close-authority / any unknown extension.
pub fn vet_mint(mint: Pubkey, owner: &Pubkey, data: &[u8]) -> Result<(), TxBuilderError> {
    arb_program::token2022::vet_mint_bytes(owner, data)
        .map_err(|_| TxBuilderError::ForbiddenTokenExtension { mint })
}

/// Convenience filter type so callers can `Token2022Filter::vet_mint(..)` symmetrically with
/// the on-chain naming.
pub struct Token2022Filter;

impl Token2022Filter {
    #[inline]
    pub fn vet_mint(mint: Pubkey, owner: &Pubkey, data: &[u8]) -> Result<(), TxBuilderError> {
        vet_mint(mint, owner, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_config::program_ids::{TOKEN_2022_PROGRAM, TOKEN_PROGRAM};

    // ExtensionType discriminants (must match the on-chain scanner).
    const EXT_TRANSFER_FEE_CONFIG: u16 = 1;
    const EXT_MINT_CLOSE_AUTHORITY: u16 = 3;
    const EXT_PERMANENT_DELEGATE: u16 = 12;
    const TLV_START: usize = 166;

    fn mint(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn with_tlv(entries: &[(u16, &[u8])]) -> Vec<u8> {
        let mut v = vec![0u8; TLV_START];
        for (ty, val) in entries {
            v.extend_from_slice(&ty.to_le_bytes());
            v.extend_from_slice(&(val.len() as u16).to_le_bytes());
            v.extend_from_slice(val);
        }
        v
    }

    #[test]
    fn plain_spl_mint_passes() {
        assert!(vet_mint(mint(9), &TOKEN_PROGRAM, &[0u8; 82]).is_ok());
    }

    #[test]
    fn fee_only_token2022_passes() {
        let data = with_tlv(&[(EXT_TRANSFER_FEE_CONFIG, &[0u8; 108])]);
        assert!(vet_mint(mint(9), &TOKEN_2022_PROGRAM, &data).is_ok());
    }

    #[test]
    fn forbidden_extension_is_rejected_with_mint() {
        for ty in [EXT_MINT_CLOSE_AUTHORITY, EXT_PERMANENT_DELEGATE] {
            let data = with_tlv(&[(ty, &[0u8; 32])]);
            let err = vet_mint(mint(7), &TOKEN_2022_PROGRAM, &data).unwrap_err();
            assert_eq!(
                err,
                TxBuilderError::ForbiddenTokenExtension { mint: mint(7) }
            );
        }
    }
}
