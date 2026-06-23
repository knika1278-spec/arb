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

// ---- Fase 2.5 scope-expansion venues (gated by M1-GATE-EXT) ----
// These are real, verified mainnet programs, but they are NOT in the mainnet-safe Wave-1
// allowlist: each is gated behind its own per-venue both-direction differential (M1-GATE-EXT)
// and is recognized only for building/sizing/testing until that gate is green. They live in a
// SEPARATE allowlist so the Wave-1 purity invariant (and the strict mainnet boundary) is intact.
/// Meteora DLMM (LB CLMM; discretized constant-sum bins).
pub const METEORA_DLMM: Pubkey = pubkey!("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
/// Meteora DAMM v2 / CP-AMM (constant-product; Token-2022 fee path).
pub const METEORA_DAMM_V2: Pubkey = pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
/// Raydium CLMM (sqrtPriceX64 concentrated liquidity).
pub const RAYDIUM_CLMM: Pubkey = pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

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

/// The Fase-2.5 scope-expansion allowlist — Meteora DLMM, Meteora DAMM v2, Raydium CLMM, in
/// this order. SEPARATE from Wave-1: gated by `M1-GATE-EXT`, never mainnet-eligible until each
/// venue's differential is green. Disjoint from [`WAVE1_DEX_ALLOWLIST`].
pub const FASE25_DEX_ALLOWLIST: [Pubkey; 3] = [METEORA_DLMM, METEORA_DAMM_V2, RAYDIUM_CLMM];

/// `const`-evaluable membership test against [`WAVE1_DEX_ALLOWLIST`] — the strict, mainnet-safe
/// boundary. The on-chain trust-boundary check and the off-chain tx-shape validator both call
/// this. Fase-2.5 venues are intentionally NOT accepted here (see [`is_fase25_swap_program`] /
/// [`is_executable_swap_program`]).
pub const fn is_allowlisted_swap_program(program_id: &Pubkey) -> bool {
    matches!(*program_id, RAYDIUM_CPMM | ORCA_WHIRLPOOL | PUMPSWAP_AMM)
}

/// `const`-evaluable membership test against [`FASE25_DEX_ALLOWLIST`] (the gated scope-expansion
/// venues). True only for Meteora DLMM / DAMM v2 / Raydium CLMM.
pub const fn is_fase25_swap_program(program_id: &Pubkey) -> bool {
    matches!(*program_id, METEORA_DLMM | METEORA_DAMM_V2 | RAYDIUM_CLMM)
}

/// The union recognized for building/sizing/testing a route: Wave-1 (mainnet-safe) **or**
/// Fase-2.5 (gated). Use this where a route across the new venues must be recognized; use
/// [`is_allowlisted_swap_program`] where the strict mainnet boundary is required.
pub const fn is_executable_swap_program(program_id: &Pubkey) -> bool {
    is_allowlisted_swap_program(program_id) || is_fase25_swap_program(program_id)
}

/// Verification status of a pinned id, for the config↔Solscan cross-check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramIdStatus {
    /// Verified on Solscan on the given date (operator step).
    Verified { solscan_checked_on: &'static str },
    /// Fase-2.5 scope-expansion venue: a real verified program, but gated by `M1-GATE-EXT` and
    /// only ever in [`FASE25_DEX_ALLOWLIST`] (never the mainnet-safe Wave-1 allowlist).
    Fase25Gated { solscan_checked_on: &'static str },
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
    /// Whether this id may legally appear in [`FASE25_DEX_ALLOWLIST`] (gated scope expansion).
    pub fase25_swap_venue: bool,
}

/// The full pinned table. `loader::validate` cross-checks the TOML against this and asserts
/// the allowlist-purity invariant: every `wave1_swap_venue` row is `Verified`, and no other
/// row appears in `WAVE1_DEX_ALLOWLIST`.
pub const PROGRAM_ID_TABLE: &[ProgramIdEntry] = &[
    ProgramIdEntry {
        name: "raydium_cpmm",
        id: RAYDIUM_CPMM,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: true,
        fase25_swap_venue: false,
    },
    ProgramIdEntry {
        name: "orca_whirlpool",
        id: ORCA_WHIRLPOOL,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: true,
        fase25_swap_venue: false,
    },
    ProgramIdEntry {
        name: "pumpswap_amm",
        id: PUMPSWAP_AMM,
        status: ProgramIdStatus::Verified {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: true,
        fase25_swap_venue: false,
    },
    // ---- Fase 2.5 (gated by M1-GATE-EXT) ----
    ProgramIdEntry {
        name: "meteora_dlmm",
        id: METEORA_DLMM,
        status: ProgramIdStatus::Fase25Gated {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: false,
        fase25_swap_venue: true,
    },
    ProgramIdEntry {
        name: "meteora_damm_v2",
        id: METEORA_DAMM_V2,
        status: ProgramIdStatus::Fase25Gated {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: false,
        fase25_swap_venue: true,
    },
    ProgramIdEntry {
        name: "raydium_clmm",
        id: RAYDIUM_CLMM,
        status: ProgramIdStatus::Fase25Gated {
            solscan_checked_on: "PENDING_OPERATOR",
        },
        wave1_swap_venue: false,
        fase25_swap_venue: true,
    },
    ProgramIdEntry {
        name: "raydium_amm_v4",
        id: RAYDIUM_AMM_V4,
        status: ProgramIdStatus::DeferredWave2,
        wave1_swap_venue: false,
        fase25_swap_venue: false,
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
            // No id is ever in both sets.
            assert!(
                !(e.wave1_swap_venue && e.fase25_swap_venue),
                "{} cannot be both Wave-1 and Fase-2.5",
                e.name
            );
            if e.wave1_swap_venue {
                assert!(
                    matches!(e.status, ProgramIdStatus::Verified { .. }),
                    "{} is a wave1 venue but not Verified",
                    e.name
                );
                assert!(
                    is_allowlisted_swap_program(&e.id),
                    "{} missing from Wave-1 allowlist",
                    e.name
                );
            } else {
                assert!(
                    !is_allowlisted_swap_program(&e.id),
                    "{} must not be in the mainnet-safe Wave-1 allowlist",
                    e.name
                );
            }
            if e.fase25_swap_venue {
                assert!(
                    matches!(e.status, ProgramIdStatus::Fase25Gated { .. }),
                    "{} is a fase25 venue but not Fase25Gated",
                    e.name
                );
                assert!(
                    is_fase25_swap_program(&e.id),
                    "{} missing from Fase-2.5 allowlist",
                    e.name
                );
            } else {
                assert!(
                    !is_fase25_swap_program(&e.id),
                    "{} must not be in the Fase-2.5 allowlist",
                    e.name
                );
            }
        }
    }

    #[test]
    fn fase25_allowlist_is_gated_and_disjoint_from_wave1() {
        assert_eq!(
            FASE25_DEX_ALLOWLIST,
            [METEORA_DLMM, METEORA_DAMM_V2, RAYDIUM_CLMM]
        );
        // Each Fase-2.5 venue is gated: recognized as executable, but NOT on the mainnet-safe
        // Wave-1 boundary.
        for pid in FASE25_DEX_ALLOWLIST {
            assert!(is_fase25_swap_program(&pid));
            assert!(is_executable_swap_program(&pid));
            assert!(
                !is_allowlisted_swap_program(&pid),
                "Fase-2.5 venue must not pass the strict Wave-1 boundary"
            );
        }
        // The two allowlists are disjoint.
        for w in WAVE1_DEX_ALLOWLIST {
            assert!(!is_fase25_swap_program(&w));
            assert!(is_executable_swap_program(&w));
        }
        // A random id is in neither.
        let junk = Pubkey::new_from_array([9u8; 32]);
        assert!(!is_executable_swap_program(&junk));
    }
}
