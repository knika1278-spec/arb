//! Pre-build route vetting (txbuilder-11) — the off-chain layer of the trust boundary, run
//! before a tx is ever assembled/signed. Mirrors the on-chain checks (single source:
//! `arb_config::program_ids` allowlist + `arb_program::token2022`) so all three layers
//! (on-chain processor, signer tx-shape validator, this builder) agree (invariant §6):
//!
//! 1. every swap-CPI target is in the Wave-1 DEX allowlist;
//! 2. every routed mint passes the Token-2022 HARD-REJECT filter;
//! 3. every profit-checked/destination token account is the bot-owned ATA (dest = own ATA);
//! 4. no routed token account is frozen (the leg would fail on-chain).

use crate::txbuilder::error::TxBuilderError;
use crate::txbuilder::token2022::Token2022Filter;
use crate::txbuilder::wsol::derive_ata;
use arb_config::program_ids::is_allowlisted_swap_program;
use solana_pubkey::Pubkey;

/// SPL token-account `state` byte offset (0 = uninitialized, 1 = initialized, 2 = frozen).
const TOKEN_ACCOUNT_STATE_OFFSET: usize = 108;
const STATE_FROZEN: u8 = 2;

/// True if a token account's raw data marks it frozen.
pub fn is_frozen(account_data: &[u8]) -> bool {
    account_data.get(TOKEN_ACCOUNT_STATE_OFFSET) == Some(&STATE_FROZEN)
}

/// A mint to vet: its pubkey, owning token program, and raw account bytes.
pub struct MintInput<'a> {
    pub mint: Pubkey,
    pub owner_program: Pubkey,
    pub data: &'a [u8],
}

/// A token account whose state we check (must not be frozen).
pub struct TokenAccountInput<'a> {
    pub account: Pubkey,
    pub data: &'a [u8],
}

/// A claim that `claimed` is the bot's own ATA for `(mint, token_program)`.
pub struct OwnedAtaClaim {
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub claimed: Pubkey,
}

/// Everything the builder vets before assembling a route.
pub struct RouteVetInput<'a> {
    pub authority: Pubkey,
    pub swap_programs: &'a [Pubkey],
    pub mints: &'a [MintInput<'a>],
    pub token_accounts: &'a [TokenAccountInput<'a>],
    pub owned_atas: &'a [OwnedAtaClaim],
}

/// Verify a single claimed ATA equals the deterministic derivation for the authority.
pub fn verify_owned_ata(
    authority: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    claimed: &Pubkey,
) -> Result<(), TxBuilderError> {
    let expected = derive_ata(authority, mint, token_program);
    if expected == *claimed {
        Ok(())
    } else {
        Err(TxBuilderError::UnownedDestination {
            account: *claimed,
            expected,
        })
    }
}

/// Run the full pre-build vetting. Returns the first failure.
pub fn vet_route(input: &RouteVetInput) -> Result<(), TxBuilderError> {
    for program in input.swap_programs {
        if !is_allowlisted_swap_program(program) {
            return Err(TxBuilderError::UnauthorizedSwapProgram { program: *program });
        }
    }
    for m in input.mints {
        Token2022Filter::vet_mint(m.mint, &m.owner_program, m.data)?;
    }
    for ta in input.token_accounts {
        if is_frozen(ta.data) {
            return Err(TxBuilderError::FrozenAccount {
                account: ta.account,
            });
        }
    }
    for claim in input.owned_atas {
        verify_owned_ata(
            &input.authority,
            &claim.mint,
            &claim.token_program,
            &claim.claimed,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_config::program_ids::{NATIVE_MINT, RAYDIUM_CPMM, TOKEN_PROGRAM};

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn unfrozen() -> Vec<u8> {
        let mut v = vec![0u8; 165];
        v[TOKEN_ACCOUNT_STATE_OFFSET] = 1; // initialized
        v
    }

    #[test]
    fn frozen_detection() {
        let mut data = unfrozen();
        assert!(!is_frozen(&data));
        data[TOKEN_ACCOUNT_STATE_OFFSET] = STATE_FROZEN;
        assert!(is_frozen(&data));
        assert!(!is_frozen(&[])); // too short -> not frozen (handled elsewhere)
    }

    #[test]
    fn rejects_non_allowlisted_swap_program() {
        let input = RouteVetInput {
            authority: key(1),
            swap_programs: &[key(123)],
            mints: &[],
            token_accounts: &[],
            owned_atas: &[],
        };
        assert_eq!(
            vet_route(&input).unwrap_err(),
            TxBuilderError::UnauthorizedSwapProgram { program: key(123) }
        );
    }

    #[test]
    fn rejects_frozen_account() {
        let mut data = unfrozen();
        data[TOKEN_ACCOUNT_STATE_OFFSET] = STATE_FROZEN;
        let input = RouteVetInput {
            authority: key(1),
            swap_programs: &[RAYDIUM_CPMM],
            mints: &[],
            token_accounts: &[TokenAccountInput {
                account: key(7),
                data: &data,
            }],
            owned_atas: &[],
        };
        assert_eq!(
            vet_route(&input).unwrap_err(),
            TxBuilderError::FrozenAccount { account: key(7) }
        );
    }

    #[test]
    fn owned_ata_must_match_derivation() {
        let auth = key(1);
        let good = derive_ata(&auth, &NATIVE_MINT, &TOKEN_PROGRAM);
        assert!(verify_owned_ata(&auth, &NATIVE_MINT, &TOKEN_PROGRAM, &good).is_ok());
        let bad = key(200);
        assert!(matches!(
            verify_owned_ata(&auth, &NATIVE_MINT, &TOKEN_PROGRAM, &bad),
            Err(TxBuilderError::UnownedDestination { .. })
        ));
    }

    #[test]
    fn full_route_passes_when_clean() {
        let auth = key(1);
        let base_ata = derive_ata(&auth, &NATIVE_MINT, &TOKEN_PROGRAM);
        let input = RouteVetInput {
            authority: auth,
            swap_programs: &[RAYDIUM_CPMM],
            mints: &[MintInput {
                mint: NATIVE_MINT,
                owner_program: TOKEN_PROGRAM,
                data: &[0u8; 82],
            }],
            token_accounts: &[TokenAccountInput {
                account: base_ata,
                data: &unfrozen(),
            }],
            owned_atas: &[OwnedAtaClaim {
                mint: NATIVE_MINT,
                token_program: TOKEN_PROGRAM,
                claimed: base_ata,
            }],
        };
        assert!(vet_route(&input).is_ok());
    }
}
