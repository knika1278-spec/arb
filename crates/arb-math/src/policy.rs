//! Sizing policy: trade a deliberate fraction (90–95%) of the optimum (plan.md §7).
//! Rationale is a buffer for integer-rounding + opportunity-decay between detect and land,
//! NOT latency undershoot — and every miss is reverted by the on-chain assert. For
//! latency-bound liquid pairs a more aggressive undershoot (~60%) may fit; choose per-niche.

/// Fraction of optimum to actually size, in basis points (9000 = 90%, 9500 = 95%).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizingPolicy {
    pub fraction_bps: u32,
}

impl SizingPolicy {
    /// Default Milestone-1 policy: 92.5% of optimum (mid of the 90–95% band).
    pub const DEFAULT: SizingPolicy = SizingPolicy { fraction_bps: 9250 };

    pub const fn new(fraction_bps: u32) -> Self {
        Self { fraction_bps }
    }

    /// Apply the policy to an optimal size. Saturates rather than overflowing; never
    /// returns more than `optimal`.
    pub fn apply(&self, optimal: u64) -> u64 {
        let bps = self.fraction_bps.min(10_000) as u128;
        let scaled = (optimal as u128).saturating_mul(bps) / 10_000;
        scaled.min(optimal as u128) as u64
    }
}

impl Default for SizingPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn applies_fraction() {
        assert_eq!(SizingPolicy::new(9000).apply(1000), 900);
        assert_eq!(SizingPolicy::new(9500).apply(1000), 950);
        assert_eq!(SizingPolicy::DEFAULT.apply(10_000), 9250);
    }

    #[test]
    fn never_exceeds_optimum_and_caps_bps() {
        assert_eq!(SizingPolicy::new(20_000).apply(1000), 1000); // bps capped at 10000
        assert_eq!(SizingPolicy::new(9250).apply(0), 0);
    }
}
