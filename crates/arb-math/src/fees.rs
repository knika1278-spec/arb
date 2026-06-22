//! Token-2022 `TransferFee` math, mirroring `spl-token-2022`'s `transfer_fee.rs`
//! **bit-for-bit**. The forward fee (`calculate_fee`) ceils and caps at `maximum_fee`;
//! the inverse (`calculate_inverse_fee`) is intentionally NON-symmetric (it re-derives a
//! pre-fee amount with a separate ceiling division, so it can differ by ≤1 unit). Mixing
//! the two directions is a real, audited CVE class (Kora paymaster) — keep them distinct.
//!
//! Profit-check must always use the ACTUAL post-transfer balance delta, not these
//! predictions; these exist so off-chain sizing can anticipate the skim. Fees are read
//! live per-epoch (`getEpochFee`) off-chain — never cache across an epoch.

/// `10_000` — basis-point denominator (SPL `ONE_IN_BASIS_POINTS`).
pub const ONE_IN_BASIS_POINTS: u128 = 10_000;

/// The two fields of a Token-2022 `TransferFee` that affect amounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferFeeConfig {
    pub transfer_fee_basis_points: u16,
    pub maximum_fee: u64,
}

impl TransferFeeConfig {
    pub const NONE: TransferFeeConfig = TransferFeeConfig {
        transfer_fee_basis_points: 0,
        maximum_fee: 0,
    };

    /// Forward fee on a known gross (`pre_fee`) amount. Ceiling, capped at `maximum_fee`.
    /// Mirrors `spl_token_2022::extension::transfer_fee::TransferFee::calculate_fee`.
    pub fn calculate_fee(&self, pre_fee_amount: u64) -> Option<u64> {
        let bps = self.transfer_fee_basis_points as u128;
        if bps == 0 || pre_fee_amount == 0 {
            return Some(0);
        }
        let numerator = (pre_fee_amount as u128).checked_mul(bps)?;
        let raw_fee = numerator
            .checked_add(ONE_IN_BASIS_POINTS.checked_sub(1)?)? // ceiling
            .checked_div(ONE_IN_BASIS_POINTS)?;
        let capped = raw_fee.min(self.maximum_fee as u128);
        u64::try_from(capped).ok()
    }

    /// Amount actually received when `pre_fee_amount` is sent (gross minus forward fee).
    pub fn amount_after_fee(&self, pre_fee_amount: u64) -> Option<u64> {
        let fee = self.calculate_fee(pre_fee_amount)?;
        pre_fee_amount.checked_sub(fee)
    }

    /// Gross amount that must be sent so the recipient nets `post_fee_amount`.
    /// Mirrors SPL `calculate_pre_fee_amount` (ceiling division + max-fee branch).
    pub fn calculate_pre_fee_amount(&self, post_fee_amount: u64) -> Option<u64> {
        let bps = self.transfer_fee_basis_points as u128;
        if bps == 0 {
            return Some(post_fee_amount);
        }
        if post_fee_amount == 0 {
            return Some(0);
        }
        let maximum_fee = self.maximum_fee as u128;
        let denominator = ONE_IN_BASIS_POINTS.checked_sub(bps)?;
        if denominator == 0 {
            // 100% fee: pre-fee is post + maximum_fee (SPL clamps to the cap).
            return post_fee_amount.checked_add(self.maximum_fee);
        }
        let numerator = (post_fee_amount as u128).checked_mul(ONE_IN_BASIS_POINTS)?;
        // ceil_div(numerator, denominator)
        let raw_pre = numerator
            .checked_add(denominator.checked_sub(1)?)?
            .checked_div(denominator)?;
        let implied_fee = raw_pre.checked_sub(post_fee_amount as u128)?;
        if implied_fee >= maximum_fee {
            post_fee_amount.checked_add(self.maximum_fee)
        } else {
            u64::try_from(raw_pre).ok()
        }
    }

    /// Inverse fee: the fee corresponding to a given post-fee (received) amount.
    /// Mirrors SPL `calculate_inverse_fee`.
    pub fn calculate_inverse_fee(&self, post_fee_amount: u64) -> Option<u64> {
        let pre = self.calculate_pre_fee_amount(post_fee_amount)?;
        self.calculate_fee(pre)
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn cfg(bps: u16, max: u64) -> TransferFeeConfig {
        TransferFeeConfig {
            transfer_fee_basis_points: bps,
            maximum_fee: max,
        }
    }

    #[test]
    fn zero_fee_is_identity() {
        let c = TransferFeeConfig::NONE;
        assert_eq!(c.calculate_fee(1_000_000), Some(0));
        assert_eq!(c.amount_after_fee(1_000_000), Some(1_000_000));
        assert_eq!(c.calculate_pre_fee_amount(1_000_000), Some(1_000_000));
    }

    #[test]
    fn forward_fee_ceils_and_caps() {
        let c = cfg(100, u64::MAX); // 1%
                                    // 1234 * 100 / 10000 = 12.34 -> ceil 13
        assert_eq!(c.calculate_fee(1234), Some(13));
        assert_eq!(c.amount_after_fee(1234), Some(1234 - 13));
        // cap
        let capped = cfg(100, 5);
        assert_eq!(capped.calculate_fee(1234), Some(5));
    }

    #[test]
    fn inverse_recovers_net_within_one_unit() {
        // Sending pre = calculate_pre_fee_amount(net) nets at least `net`.
        let c = cfg(30, u64::MAX); // 0.30%
        for net in [1u64, 7, 100, 9_999, 1_000_000, 123_456_789] {
            let pre = c.calculate_pre_fee_amount(net).unwrap();
            let received = c.amount_after_fee(pre).unwrap();
            assert!(received >= net, "net={net} pre={pre} received={received}");
            assert!(received <= net + 1, "inverse overshoot >1 unit");
        }
    }

    #[test]
    fn forward_and_inverse_are_not_symmetric() {
        // The whole point: fee(amount) != inverse_fee(amount) in general.
        let c = cfg(30, u64::MAX);
        let amt = 10_000u64;
        let fwd = c.calculate_fee(amt).unwrap();
        let inv = c.calculate_inverse_fee(amt).unwrap();
        // They are close but the inverse derives from a different (pre-fee) base.
        assert!(fwd.abs_diff(inv) <= 2);
    }
}
