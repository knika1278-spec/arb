//! Shared metric value types: route identity, drop/revert causes, and the tx economic outcome.
//! These cross the executor → observ → signer boundaries (plan §4), so they live in one place.

use arb_types::SwapDir;
use solana_pubkey::Pubkey;

/// Identifies a venue pair + direction for per-route metric bucketing (`realized_slippage`,
/// `p_land` EWMA). `venue_pair` is the two pool/program pubkeys in canonical (leg-A, leg-B)
/// order; `direction` is leg-A's orientation. Distinct routes bucket independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RouteKey {
    pub venue_pair: [Pubkey; 2],
    pub direction: SwapDir,
}

impl RouteKey {
    pub fn new(leg_a: Pubkey, leg_b: Pubkey, direction: SwapDir) -> Self {
        Self {
            venue_pair: [leg_a, leg_b],
            direction,
        }
    }
}

/// Best-effort attribution of why an attempt did not land profitably. Causes are co-dominant
/// (plan §6 "jangan asumsikan stale blockhash satu-satunya penyebab"); the loop records one per
/// attempt. Mirrors the executor `DropCause` but is the economic-side coarse classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevertCause {
    /// Lost the Jito tip-per-CU auction.
    TipLost,
    /// Block/region congestion.
    Congestion,
    /// Blockhash went stale before inclusion.
    StaleBlockhash,
    /// Pre-tip simulation failed (never relied on for fee protection).
    SimFail,
    /// Landed on-chain but the terminal assert reverted (`Unprofitable`) — the opportunity
    /// decayed between detection and execution. Burns base+priority.
    OnchainUnprofitable,
    /// Cause could not be determined.
    Unknown,
}

impl RevertCause {
    /// Stable label for metric keys / dashboards.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TipLost => "tip_lost",
            Self::Congestion => "congestion",
            Self::StaleBlockhash => "stale_blockhash",
            Self::SimFail => "sim_fail",
            Self::OnchainUnprofitable => "onchain_unprofitable",
            Self::Unknown => "unknown",
        }
    }

    /// Did the attempt actually land on-chain (and therefore burn base+priority)? Only an
    /// on-chain revert burns fees; auction/sim drops never reach a block (plan §2 "biaya nol").
    pub const fn burned_fees(self) -> bool {
        matches!(self, Self::OnchainUnprofitable)
    }
}

/// Economic classification of a single attempt's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxKind {
    /// Landed profitably.
    Landed,
    /// Reached a block and the terminal assert reverted (burns base+priority).
    Reverted,
    /// Dropped BEFORE block inclusion (lost auction / stale) — zero cost, a normal competitive
    /// loss. Kept OUT of the >30% infra-bug revert-rate (plan §6: co-dominant drop causes are not
    /// an infra signal), unlike an on-chain `Reverted`.
    Dropped,
}
