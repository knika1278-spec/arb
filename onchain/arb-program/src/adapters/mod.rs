//! Swap-CPI adapters. The account metas for each leg are forwarded **generically** from the
//! untrusted `remaining_accounts` (preserving each account's signer/writable flags); the
//! only venue-specific part is the swap instruction-data encoding. Every CPI target is
//! trust-checked against the allowlist before invoke (invariant §6).
//!
//! ⚠️ DISCRIMINATOR STATUS: the per-venue 8-byte Anchor discriminators below are
//! **the Anchor sha256 discriminators (filled 2026-06-22; pending M1-GATE proof)**. They MUST be filled from each program's IDL and proven by
//! the M1-GATE differential test on Surfpool/LiteSVM (which needs `cargo build-sbf`) before
//! any mainnet send. They are isolated in `discriminators` so this is auditable.

pub mod orca_whirlpool;
pub mod pumpswap;
pub mod raydium_cpmm;

use crate::trust::verify_swap_program;
use arb_types::DexKind;
use solana_program::account_info::AccountInfo;
use solana_program::entrypoint::ProgramResult;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::program::invoke;

/// Everything one swap leg needs to issue its CPI.
pub struct LegContext<'a, 'info> {
    /// The DEX program account to invoke (verified allowlisted).
    pub dex_program: &'a AccountInfo<'info>,
    /// Accounts the CPI consumes, in the venue's canonical order (client-supplied).
    pub swap_accounts: &'a [AccountInfo<'info>],
    pub amount_in: u64,
    pub min_out: u64,
}

/// Encode the venue-specific swap instruction data (discriminator + amount_in + min_out).
pub fn encode_swap_data(dex: DexKind, amount_in: u64, min_out: u64) -> alloc_vec::Vec<u8> {
    match dex {
        DexKind::RaydiumCpmm => raydium_cpmm::encode(amount_in, min_out),
        DexKind::OrcaWhirlpool => orca_whirlpool::encode(amount_in, min_out),
        DexKind::PumpSwapAmm => pumpswap::encode(amount_in, min_out),
    }
}

/// Build + invoke one swap leg's CPI after trust-checking the target program.
pub fn invoke_swap(dex: DexKind, ctx: &LegContext) -> ProgramResult {
    verify_swap_program(ctx.dex_program.key)?;
    let data = encode_swap_data(dex, ctx.amount_in, ctx.min_out);
    let metas: alloc_vec::Vec<AccountMeta> = ctx.swap_accounts.iter().map(to_meta).collect();
    let ix = Instruction {
        program_id: *ctx.dex_program.key,
        accounts: metas,
        data,
    };
    invoke(&ix, ctx.swap_accounts)
}

fn to_meta(ai: &AccountInfo) -> AccountMeta {
    AccountMeta {
        pubkey: *ai.key,
        is_signer: ai.is_signer,
        is_writable: ai.is_writable,
    }
}

/// Helper to assemble `[discriminator][amount_in LE][min_out LE]`.
pub(crate) fn encode_with_discriminator(
    disc: &[u8; 8],
    amount_in: u64,
    min_out: u64,
) -> alloc_vec::Vec<u8> {
    let mut v = alloc_vec::Vec::with_capacity(8 + 16);
    v.extend_from_slice(disc);
    v.extend_from_slice(&amount_in.to_le_bytes());
    v.extend_from_slice(&min_out.to_le_bytes());
    v
}

/// Re-export std `Vec` under a stable path so adapter code reads cleanly whether or not the
/// crate is later split for no_std (the program runs with an allocator on-chain).
pub(crate) mod alloc_vec {
    pub use std::vec::Vec;
}

/// Sanity: a fake (non-allowlisted) program id is rejected before any CPI is built.
#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::pubkey::Pubkey;

    #[test]
    fn non_allowlisted_program_rejected() {
        let fake = Pubkey::new_from_array([5u8; 32]);
        assert!(verify_swap_program(&fake).is_err());
    }

    #[test]
    fn encodes_data_layout() {
        let d = encode_swap_data(DexKind::RaydiumCpmm, 1000, 990);
        assert_eq!(d.len(), 24); // 8 disc + 8 + 8
        assert_eq!(&d[8..16], &1000u64.to_le_bytes());
        assert_eq!(&d[16..24], &990u64.to_le_bytes());
    }
}
