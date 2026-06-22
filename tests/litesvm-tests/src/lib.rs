//! Test-substrate crate. The actual LiteSVM program-loading + revert proof lands in Fase 1
//! (needs the `cargo build-sbf` artifact). This lib only exists so the smoke test below has
//! a home and the workspace has a wired test crate from day 1.

/// Marker so the crate is non-empty and links the shared config.
pub fn substrate_ready() -> bool {
    !arb_config::WAVE1_DEX_ALLOWLIST.is_empty()
}
