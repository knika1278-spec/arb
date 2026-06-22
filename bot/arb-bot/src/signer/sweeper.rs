//! signer-8 — blast-radius sweeper (decision logic).
//!
//! Caps how much value a compromised hot key can lose: on a cron tick or when the hot balance
//! exceeds `hot_cap_lamports`, sweep the surplus (`balance − working_reserve − rent_exempt_min`) to
//! the cold treasury — the ONLY allowlisted sweep destination. The decision never drops the hot
//! balance below `working_reserve + rent_exempt_min`. Sweeps are permitted even during a kill-switch
//! halt (containment), via a dedicated sweep-sign path that validates `dest == treasury`; arb signs
//! stay blocked. This module owns the pure decision; the async cron task + RPC submit are a seam.

use solana_pubkey::Pubkey;

/// Sweeper config (loaded from `ops/config/signer.toml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SweeperConfig {
    /// Minimum working capital kept on the hot key.
    pub working_reserve_lamports: u64,
    /// Rent-exempt minimum that must never be swept.
    pub rent_exempt_min_lamports: u64,
    /// Balance above which a threshold-triggered sweep fires.
    pub hot_cap_lamports: u64,
    /// The cold treasury — the sole allowlisted sweep destination.
    pub treasury: Pubkey,
}

impl SweeperConfig {
    /// The floor the hot balance must never drop below.
    pub fn floor(&self) -> u64 {
        self.working_reserve_lamports
            .saturating_add(self.rent_exempt_min_lamports)
    }

    /// Whether `dest` is the allowlisted sweep destination (the treasury). The sweep-sign path
    /// rejects anything else even if the hot key were compromised.
    pub fn is_valid_sweep_dest(&self, dest: &Pubkey) -> bool {
        *dest == self.treasury
    }
}

/// What fired the sweep evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepTrigger {
    /// Periodic cron tick — sweep any surplus.
    Cron,
    /// Balance crossed `hot_cap_lamports` — only sweep if over the cap.
    BalanceThreshold,
}

/// The sweep decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepDecision {
    /// Transfer `amount` to the treasury.
    Sweep { amount: u64, dest: Pubkey },
    /// Nothing to sweep.
    Hold,
}

/// Decide whether/how much to sweep given the current hot `balance`.
pub fn decide_sweep(cfg: &SweeperConfig, balance: u64, trigger: SweepTrigger) -> SweepDecision {
    // Threshold trigger only acts once the balance is over the hot cap.
    if trigger == SweepTrigger::BalanceThreshold && balance <= cfg.hot_cap_lamports {
        return SweepDecision::Hold;
    }
    let surplus = balance.saturating_sub(cfg.floor());
    if surplus == 0 {
        SweepDecision::Hold
    } else {
        SweepDecision::Sweep {
            amount: surplus,
            dest: cfg.treasury,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SweeperConfig {
        SweeperConfig {
            working_reserve_lamports: 1_000_000,
            rent_exempt_min_lamports: 2_000,
            hot_cap_lamports: 5_000_000,
            treasury: Pubkey::new_from_array([42; 32]),
        }
    }

    #[test]
    fn cron_sweeps_surplus_above_floor() {
        let c = cfg();
        // balance 3_002_000; floor = 1_002_000 => surplus 2_000_000.
        match decide_sweep(&c, 3_002_000, SweepTrigger::Cron) {
            SweepDecision::Sweep { amount, dest } => {
                assert_eq!(amount, 2_000_000);
                assert_eq!(dest, c.treasury);
            }
            _ => panic!("expected a sweep"),
        }
    }

    #[test]
    fn never_drops_below_floor() {
        let c = cfg();
        // After a sweep, remaining == floor exactly.
        if let SweepDecision::Sweep { amount, .. } = decide_sweep(&c, 3_002_000, SweepTrigger::Cron)
        {
            assert_eq!(3_002_000 - amount, c.floor());
        }
        // At/below floor => nothing to sweep.
        assert_eq!(
            decide_sweep(&c, c.floor(), SweepTrigger::Cron),
            SweepDecision::Hold
        );
        assert_eq!(
            decide_sweep(&c, 500, SweepTrigger::Cron),
            SweepDecision::Hold
        );
    }

    #[test]
    fn threshold_trigger_waits_for_hot_cap() {
        let c = cfg();
        // Below the hot cap => threshold trigger holds even though there is surplus.
        assert_eq!(
            decide_sweep(&c, 3_002_000, SweepTrigger::BalanceThreshold),
            SweepDecision::Hold
        );
        // Above the hot cap => sweep down to the floor.
        match decide_sweep(&c, 6_000_000, SweepTrigger::BalanceThreshold) {
            SweepDecision::Sweep { amount, .. } => assert_eq!(amount, 6_000_000 - c.floor()),
            _ => panic!("expected a sweep"),
        }
    }

    #[test]
    fn only_treasury_is_a_valid_sweep_dest() {
        let c = cfg();
        assert!(c.is_valid_sweep_dest(&c.treasury));
        assert!(!c.is_valid_sweep_dest(&Pubkey::new_from_array([9; 32])));
    }
}
