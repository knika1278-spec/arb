//! add-5 — runtime SIMD-0268 / SIMD-0339 feature-gate state + CU-per-CPI budget.
//!
//! plan.md §6 is explicit: **read the feature-gate activation at runtime — never hardcode** the
//! pre/post CPI-depth (5 vs 9) or max CPI account-infos (128 vs 255). This module owns the *mapping*
//! from an activation state to the concrete budget; the activation read itself is a seam (the bot
//! reads via RPC `getFeatureActivation`; the on-chain program reads the active feature set). Until
//! the read says otherwise, [`FeatureGateState::default`] is the conservative PRE-activation budget,
//! so we never over-promise CPI depth/account budget the runtime won't grant.
//!
//! `no_std` (pure const mapping) so the on-chain program and the off-chain bot share one definition.

/// Runtime activation state of the two relevant SIMDs. Filled by a runtime read (RPC / feature
/// sysvar) — NOT hardcoded. Default = both inactive (the safe lower budget).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FeatureGateState {
    /// SIMD-0268: raises the CPI invoke-depth limit from 5 to 9.
    pub simd_0268_active: bool,
    /// SIMD-0339: lowers CU/CPI (1000→946) and raises max CPI account-infos (128→255).
    pub simd_0339_active: bool,
}

/// The concrete per-CPI budget selected from an activation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpiBudget {
    /// Maximum CPI invoke depth.
    pub max_cpi_depth: u8,
    /// Base compute units charged per CPI.
    pub cu_per_cpi: u32,
    /// Maximum account-infos passable across a CPI.
    pub max_cpi_account_infos: u16,
}

impl CpiBudget {
    /// Select the budget for an activation state (the ONLY place the 5/9 · 1000/946 · 128/255
    /// numbers live — callers must derive from this, never re-hardcode).
    pub const fn from_features(s: FeatureGateState) -> Self {
        Self {
            max_cpi_depth: if s.simd_0268_active { 9 } else { 5 },
            cu_per_cpi: if s.simd_0339_active { 946 } else { 1000 },
            max_cpi_account_infos: if s.simd_0339_active { 255 } else { 128 },
        }
    }

    /// Whether a route needing `account_infos` across a single CPI fits this budget.
    pub const fn fits_cpi_account_infos(&self, account_infos: u16) -> bool {
        account_infos <= self.max_cpi_account_infos
    }

    /// Whether a route needing `depth` nested CPIs fits this budget.
    pub const fn fits_depth(&self, depth: u8) -> bool {
        depth <= self.max_cpi_depth
    }
}

impl Default for CpiBudget {
    fn default() -> Self {
        Self::from_features(FeatureGateState::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_activation_is_the_conservative_budget() {
        let b = CpiBudget::default();
        assert_eq!(b.max_cpi_depth, 5);
        assert_eq!(b.cu_per_cpi, 1000);
        assert_eq!(b.max_cpi_account_infos, 128);
    }

    #[test]
    fn simd_0268_raises_cpi_depth() {
        let b = CpiBudget::from_features(FeatureGateState {
            simd_0268_active: true,
            simd_0339_active: false,
        });
        assert_eq!(b.max_cpi_depth, 9);
        // 0339 still inactive => old CU/account-info numbers.
        assert_eq!(b.cu_per_cpi, 1000);
        assert_eq!(b.max_cpi_account_infos, 128);
    }

    #[test]
    fn simd_0339_lowers_cu_and_raises_account_infos() {
        let b = CpiBudget::from_features(FeatureGateState {
            simd_0268_active: true,
            simd_0339_active: true,
        });
        assert_eq!(b.cu_per_cpi, 946);
        assert_eq!(b.max_cpi_account_infos, 255);
        assert!(b.fits_cpi_account_infos(255));
        assert!(!b.fits_cpi_account_infos(256));
        assert!(b.fits_depth(9) && !b.fits_depth(10));
    }
}
