//! `no_std` const table of every pinned program id, plus the canonical Wave-1 swap-program
//! allowlist and a `const`-evaluable membership test. This is shared verbatim by the
//! on-chain program (`onchain/allowlist.rs` mirrors it) and the off-chain bot, so the
//! trust boundary cannot drift between where it is enforced and where it is built.
//!
//! Only **Verified** Wave-1 venues appear in [`WAVE1_DEX_ALLOWLIST`]. Raydium AMM v4 is
//! present but `DeferredWave2`. Proprietary "dark" AMMs (HumidiFi/Tessera/GoonFi) are
//! intentionally absent — their ids were only partially captured in sources and must be
//! Solscan-verified before they could ever be considered (they are `Unverified` and never
//! allowlisted regardless).

use solana_pubkey::{pubkey, Pubkey};

// ---- Wave-1 swap venues (Verified) ----
/// Raydium CPMM (constant-product; Token-2022-friendly; pump.fun-era graduations).
pub const RAYDIUM_CPMM: Pubkey = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
/// Orca Whirlpool (sqrtPriceX64 CLMM; swap_v2).
pub const ORCA_WHIRLPOOL: Pubkey = pubkey!("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
/// PumpSwap AMM (where pump.fun graduations now land — Wave-1 per plan.md §4 KOREKSI).
pub const PUMPSWAP_AMM: Pubkey = pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");

// ---- Deferred (Wave 2) ----
/// Raydium AMM v4 legacy — heavy account weight; deferred to Fase 3 (Wave 2).
pub const RAYDIUM_AMM_V4: Pubkey = pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

// ---- Infrastructure programs (Verified, used by the tx-builder/ALT/WSOL paths) ----
pub const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const ASSOCIATED_TOKEN_PROGRAM: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const COMPUTE_BUDGET_PROGRAM: Pubkey = pubkey!("ComputeBudget111111111111111111111111111111");
pub const SYSTEM_PROGRAM: Pubkey = pubkey!("11111111111111111111111111111111");
pub const ADDRESS_LOOKUP_TABLE_PROGRAM: Pubkey =
    pubkey!("AddressLookupTab1e1111111111111111111111111");
/// Wrapped SOL native mint.
pub const NATIVE_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");

/// The canonical Wave-1 swap-program allowlist — exactly these three, in this order.
pub const WAVE1_DEX_ALLOWLIST: [Pubkey; 3] = [RAYDIUM_CPMM, ORCA_WHIRLPOOL, PUMPSWAP_AMM];

/// `const`-evaluable membership test against [`WAVE1_DEX_ALLOWLIST`]. The on-chain
/// trust-boundary check and the off-chain tx-shape validator both call this.
pub const fn is_allowlisted_swap_program(program_id: &Pubkey) -> bool {
    matches!(*program_id, RAYDIUM_CPMM | ORCA_WHIRLPOOL | PUMPSWAP_AMM)
}

/// Verification status of a pinned id, for the config↔Solscan cross-check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramIdStatus {
    /// Verified on-chain on the given date: `getAccountInfo` showed `executable = true` with
    /// owner `BPFLoaderUpgradeable` (PumpSwap identity also Solscan-cross-checked). The field
    /// name is historical; the check is the authoritative on-chain read that Solscan renders.
    Verified { solscan_checked_on: &'static str },
    /// Real venue, intentionally deferred to Wave 2.
    DeferredWave2,
    /// Id only partially captured from sources; never allowlisted until verified.
    Unverified,
}

/// One row of the human-auditable id table (mirrors `infra/config/program_ids.toml`).
#[derive(Clone, Copy, Debug)]
pub struct ProgramIdEntry {
    pub name: &'static str,
    pub id: Pubkey,
    pub status: ProgramIdStatus,
    /// Whether this id may legally appear in `WAVE1_DEX_ALLOWLIST`.
    pub wave1_swap_venue: bool,
}

/// The full pinned table. `loader::validate` cross-checks the TOML against this and asserts
/// the allowlist-purity invariant: every `wave1_swap_venue` row is `Verified`, and no other
/// row appears in `WAVE1_DEX_ALLOWLIST`.
pub const PROGRAM_ID_TABLE: &[ProgramIdEntry] = &[
    ProgramIdEntry {
        name: "raydium_cpmm",
        id: RAYDIUM_CPMM,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "2026-06-22",
        },
        wave1_swap_venue: true,
    },
    ProgramIdEntry {
        name: "orca_whirlpool",
        id: ORCA_WHIRLPOOL,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "2026-06-22",
        },
        wave1_swap_venue: true,
    },
    ProgramIdEntry {
        name: "pumpswap_amm",
        id: PUMPSWAP_AMM,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "2026-06-22",
        },
        wave1_swap_venue: true,
    },
    ProgramIdEntry {
        name: "raydium_amm_v4",
        id: RAYDIUM_AMM_V4,
        status: ProgramIdStatus::DeferredWave2,
        wave1_swap_venue: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_exactly_the_three_wave1_venues() {
        assert_eq!(
            WAVE1_DEX_ALLOWLIST,
            [RAYDIUM_CPMM, ORCA_WHIRLPOOL, PUMPSWAP_AMM]
        );
        assert!(is_allowlisted_swap_program(&RAYDIUM_CPMM));
        assert!(is_allowlisted_swap_program(&ORCA_WHIRLPOOL));
        assert!(is_allowlisted_swap_program(&PUMPSWAP_AMM));
    }

    #[test]
    fn deferred_and_infra_programs_are_not_allowlisted() {
        assert!(!is_allowlisted_swap_program(&RAYDIUM_AMM_V4));
        assert!(!is_allowlisted_swap_program(&TOKEN_PROGRAM));
        assert!(!is_allowlisted_swap_program(&SYSTEM_PROGRAM));
        assert!(!is_allowlisted_swap_program(&Pubkey::new_from_array(
            [7u8; 32]
        )));
    }

    #[test]
    fn allowlist_purity_invariant_holds_in_table() {
        for e in PROGRAM_ID_TABLE {
            if e.wave1_swap_venue {
                assert!(
                    matches!(e.status, ProgramIdStatus::Verified { .. }),
                    "{} is a wave1 venue but not Verified",
                    e.name
                );
                assert!(
                    is_allowlisted_swap_program(&e.id),
                    "{} missing from allowlist",
                    e.name
                );
            } else {
                assert!(
                    !is_allowlisted_swap_program(&e.id),
                    "{} must not be allowlisted",
                    e.name
                );
            }
        }
    }
}
