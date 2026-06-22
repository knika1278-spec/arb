//! `arb-config` — the single source of truth every other crate links: the pinned
//! program-id allowlist + protocol hard limits (`no_std` core, shared with the on-chain
//! program) and the data-source/landing ladder + secrets loader (`std` side, used by the
//! bot/signer). Centralizing these here is what keeps the on-chain trust boundary and the
//! off-chain tx-builder from drifting apart.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

pub mod features;
pub mod limits;
pub mod program_ids;

#[cfg(feature = "std")]
pub mod loader;
#[cfg(feature = "std")]
pub mod providers;
#[cfg(feature = "std")]
pub mod secrets;

// no_std core re-exports (linked by the on-chain program).
pub use features::{CpiBudget, FeatureGateState};
pub use limits::{
    cu_limit_with_margin, BASE_FEE_LAMPORTS_PER_SIG, CU_LIMIT_SIM_MARGIN_BPS,
    MAX_COMPUTE_UNIT_LIMIT, MAX_LOADED_ACCOUNTS, MAX_TX_ACCOUNT_LOCKS, TX_SIZE_LIMIT_BYTES,
};
pub use program_ids::{is_allowlisted_swap_program, WAVE1_DEX_ALLOWLIST};

#[cfg(feature = "std")]
pub use loader::{load, validate, ArbConfig, Cluster, ConfigError};
