//! txbuilder-13 — Jito tip instruction (Fase-2 seam) + tip capping.
//!
//! The tip is a System-program `Transfer(tip_lamports)` from the authority to a runtime-resolved
//! Jito tip account, placed **inside** the same atomic arb tx so a reverting arb pays no tip
//! (plan §2). The executor's `TipOracle` sizes + caps + selects the account; this module builds the
//! actual instruction and re-checks the profit-fraction cap as defense-in-depth (an over-tip must
//! be un-assemblable even if the oracle misbehaves). The `jitodontfront` read-only marker is placed
//! by the executor's bundle build (landing-4); here we own the tip transfer + cap.

use arb_config::program_ids::SYSTEM_PROGRAM;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::txbuilder::error::TxBuilderError;

/// System-program `Transfer` enum tag (little-endian u32) — matches `txbuilder::wsol`.
const SYSTEM_TRANSFER_TAG: u32 = 2;

/// Build the Jito tip transfer: `authority → tip_account` for `tip_lamports`. Hand-encoded to avoid
/// the deprecated `solana_program::system_instruction`.
pub fn jito_tip_ix(authority: &Pubkey, tip_account: &Pubkey, tip_lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&SYSTEM_TRANSFER_TAG.to_le_bytes());
    data.extend_from_slice(&tip_lamports.to_le_bytes());
    Instruction {
        program_id: SYSTEM_PROGRAM,
        accounts: vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(*tip_account, false),
        ],
        data,
    }
}

/// The hard tip cap: `cap = floor(cap_frac · profit)`. Returns the cap in lamports.
pub fn tip_cap(simulated_profit_lamports: u64, cap_frac: f64) -> u64 {
    (cap_frac.clamp(0.0, 1.0) * simulated_profit_lamports as f64) as u64
}

/// Build a capped Jito tip instruction. Rejects with [`TxBuilderError::TipExceedsCap`] if the tip
/// would exceed `cap_frac · simulated_profit` — the txbuilder mirror of the TipOracle cap.
pub fn build_capped_tip_ix(
    authority: &Pubkey,
    tip_account: &Pubkey,
    tip_lamports: u64,
    simulated_profit_lamports: u64,
    cap_frac: f64,
) -> Result<Instruction, TxBuilderError> {
    let cap = tip_cap(simulated_profit_lamports, cap_frac);
    if tip_lamports > cap {
        return Err(TxBuilderError::TipExceedsCap {
            tip: tip_lamports,
            cap,
            profit: simulated_profit_lamports,
        });
    }
    Ok(jito_tip_ix(authority, tip_account, tip_lamports))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn tip_ix_is_a_system_transfer_to_the_tip_account() {
        let ix = jito_tip_ix(&key(1), &key(200), 50_000);
        assert_eq!(ix.program_id, SYSTEM_PROGRAM);
        assert_eq!(ix.accounts.len(), 2);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable); // authority pays
        assert_eq!(ix.accounts[1].pubkey, key(200)); // tip account
        assert_eq!(
            u32::from_le_bytes(ix.data[0..4].try_into().unwrap()),
            SYSTEM_TRANSFER_TAG
        );
        assert_eq!(
            u64::from_le_bytes(ix.data[4..12].try_into().unwrap()),
            50_000
        );
    }

    #[test]
    fn capped_tip_within_cap_is_built() {
        // profit 100_000, cap_frac 0.5 => cap 50_000; tip 40_000 is allowed.
        let ix = build_capped_tip_ix(&key(1), &key(200), 40_000, 100_000, 0.5).unwrap();
        assert_eq!(
            u64::from_le_bytes(ix.data[4..12].try_into().unwrap()),
            40_000
        );
    }

    #[test]
    fn over_cap_tip_is_rejected() {
        // tip 60_000 > cap 50_000 => rejected.
        assert_eq!(
            build_capped_tip_ix(&key(1), &key(200), 60_000, 100_000, 0.5),
            Err(TxBuilderError::TipExceedsCap {
                tip: 60_000,
                cap: 50_000,
                profit: 100_000
            })
        );
    }

    #[test]
    fn tip_cap_floors_the_fraction() {
        assert_eq!(tip_cap(100_001, 0.5), 50_000); // floor(50_000.5)
        assert_eq!(tip_cap(0, 0.5), 0);
    }
}
