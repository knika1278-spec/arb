//! `no_std` compile-time protocol hard limits — the binding ceilings every other module
//! must respect (plan.md §6 "Hard limits & KOREKSI"). Authoritative here; `infra/config/
//! limits.toml` is only a documentation mirror that `config-check` asserts equal to these.

/// Account-lock ceiling (writable + readonly). **This is the limit that binds first**, not
/// the 256 loaded-accounts cap. Raised from 64 in v1.14.17.
pub const MAX_TX_ACCOUNT_LOCKS: usize = 128;

/// Max unique accounts a v0 tx can load (u8 index space). Each ALT holds ≤256 addresses.
pub const MAX_LOADED_ACCOUNTS: usize = 256;

/// Serialized transaction size cap, **for legacy AND v0** (1280 IPv6 MTU − 48 header). An
/// ALT compresses 32-byte keys to 1-byte indices but does NOT raise this byte cap.
pub const TX_SIZE_LIMIT_BYTES: usize = 1232;

/// Per-transaction compute-unit ceiling. Raise toward this via `SetComputeUnitLimit`.
pub const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;

/// Default CU budget per instruction before any `SetComputeUnitLimit`.
pub const DEFAULT_CU_PER_IX: u32 = 200_000;

/// Base fee per signature (lamports). 50% burned, non-refundable even on a reverted-but-
/// included tx.
pub const BASE_FEE_LAMPORTS_PER_SIG: u64 = 5_000;

/// Margin added to a *simulated* CU figure when setting `SetComputeUnitLimit` (bps).
/// Over-requesting CU = overpaying priority fee, so keep this tight (~10%).
pub const CU_LIMIT_SIM_MARGIN_BPS: u32 = 1_000;

/// Minimum Jito tip (lamports) to any of the 8 tip accounts.
pub const MIN_JITO_TIP_LAMPORTS: u64 = 1_000;

/// Max transactions in a single Jito bundle (sequential, atomic, one slot).
pub const MAX_BUNDLE_TX: usize = 5;

/// Max addresses a single ALT can hold (u8 index).
pub const ALT_MAX_ADDRESSES: usize = 256;

/// Apply the CU simulation margin: `ceil(units * (10000 + margin_bps) / 10000)`, clamped to
/// [`MAX_COMPUTE_UNIT_LIMIT`]. Saturating so it can be used in const-ish hot paths.
pub fn cu_limit_with_margin(simulated_units: u32) -> u32 {
    let scaled = (simulated_units as u64)
        .saturating_mul(10_000u64.saturating_add(CU_LIMIT_SIM_MARGIN_BPS as u64))
        .saturating_add(9_999)
        / 10_000;
    let clamped = scaled.min(MAX_COMPUTE_UNIT_LIMIT as u64);
    clamped as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_ceilings_have_the_authoritative_values() {
        assert_eq!(MAX_TX_ACCOUNT_LOCKS, 128); // NOT 256
        assert_eq!(TX_SIZE_LIMIT_BYTES, 1232);
        assert_eq!(MAX_COMPUTE_UNIT_LIMIT, 1_400_000);
        assert_eq!(MAX_LOADED_ACCOUNTS, 256);
        assert_eq!(BASE_FEE_LAMPORTS_PER_SIG, 5_000);
    }

    #[test]
    fn cu_margin_is_ten_percent_ceiled_and_clamped() {
        assert_eq!(cu_limit_with_margin(100_000), 110_000);
        assert_eq!(cu_limit_with_margin(1), 2); // ceil(1.1)
        assert_eq!(
            cu_limit_with_margin(MAX_COMPUTE_UNIT_LIMIT),
            MAX_COMPUTE_UNIT_LIMIT
        );
    }
}
