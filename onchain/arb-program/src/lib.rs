//! `arb-program` — native-Rust `TryArbitrage` on-chain program (hot path; NOT Anchor).
//!
//! A single instruction executes a 2-leg constant-product round-trip in ONE transaction and
//! ends with a terminal profit-assert; any `Err` reverts ALL state (invariant §2). The
//! program shares its DEX allowlist + hard limits with the off-chain bot via the `no_std`
//! `arb-config` core, so the trust boundary cannot drift from where the tx is built.
//!
//! Not crate-level `#![no_std]`: Solana programs build on the host (LiteSVM/Surfpool tests)
//! via `solana-program`, and the SBF entrypoint is feature-gated. The `no_std` guarantee is
//! enforced on `arb-config`'s core (what actually links into the SBF bytecode).
//!
//! STATUS (Fase 1 skeleton): instruction parsing, trust boundary, zero-copy balance reads,
//! Token-2022 vetting, and the snapshot→CPI→delta→CPI→assert control flow are implemented +
//! host-unit-tested. The per-venue CPI **discriminators are the Anchor sha256 values (filled; pending M1-GATE proof)** (see
//! `adapters`) and the end-to-end revert proof runs on Surfpool/LiteSVM once `cargo
//! build-sbf` is available — that integration is the Milestone-1 gate.

// The `solana_program::entrypoint!` macro emits `cfg(target_os = "solana")` /
// `custom-heap` / `custom-panic` checks that rustc's check-cfg flags on the host build.
// They are real SBF-target cfgs, not typos — allow them crate-wide.
#![allow(unexpected_cfgs)]

pub mod adapters;
pub mod allowlist;
pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;
pub mod token2022;
pub mod trust;

#[cfg(not(feature = "no-entrypoint"))]
mod entrypoint;

pub use allowlist::is_allowlisted_dex;
pub use arb_config::WAVE1_DEX_ALLOWLIST;
pub use instruction::{LegDescriptor, TryArbitrageData};
