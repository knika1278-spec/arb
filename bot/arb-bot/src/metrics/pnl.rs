//! observ-3 — PnL ledger + burn-rate accumulator.
//!
//! Append-only ring of per-tx economic outcomes with atomic lifetime aggregates and windowed
//! queries (burn-rate lamports/min, realized PnL, revert-rate). Time is a caller-supplied logical
//! millisecond clock (`at_millis`) so windows are deterministic in tests and decoupled from any
//! particular wall-clock source.
//!
//! Invariants enforced at `record_outcome` (plan §9): a `Reverted` outcome has `tip_paid == 0`
//! (the tip rides inside the atomic tx, so it is unpaid on revert), and a reverted tx that reached
//! a block burns exactly `prio + base` (or 0 for a pre-inclusion drop that never reached a block).

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::Mutex;

use solana_pubkey::Pubkey;

use super::types::TxKind;

/// Max retained outcomes for windowed queries (ring evicts oldest beyond this).
const RING_CAP: usize = 131_072;

/// One tx's economic outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxOutcome {
    pub kind: TxKind,
    pub token: Pubkey,
    /// Landed gross profit in lamports (≥0 on Landed; 0 on Reverted).
    pub gross_lamports: i64,
    pub prio: u64,
    pub base: u64,
    /// Tip paid — `> 0` only on Landed (tip inside atomic tx ⇒ unpaid on revert).
    pub tip_paid: u64,
    /// base+priority burned — nonzero only for a Reverted tx that reached a block.
    pub burned_lamports: u64,
    /// Logical timestamp (ms) on a monotonic caller clock.
    pub at_millis: u64,
}

impl TxOutcome {
    /// A profitable land: gross profit, fees + tip paid.
    pub fn landed(
        token: Pubkey,
        gross_lamports: i64,
        prio: u64,
        base: u64,
        tip_paid: u64,
        at_millis: u64,
    ) -> Self {
        Self {
            kind: TxKind::Landed,
            token,
            gross_lamports,
            prio,
            base,
            tip_paid,
            burned_lamports: 0,
            at_millis,
        }
    }

    /// A revert that reached a block (terminal assert fired): burns exactly base+priority, no tip.
    pub fn reverted_onchain(token: Pubkey, prio: u64, base: u64, at_millis: u64) -> Self {
        Self {
            kind: TxKind::Reverted,
            token,
            gross_lamports: 0,
            prio,
            base,
            tip_paid: 0,
            burned_lamports: prio.saturating_add(base),
            at_millis,
        }
    }

    /// A pre-inclusion drop (lost auction / stale before a block): zero cost (plan §2 "biaya nol").
    /// Classified `Dropped`, NOT `Reverted`, so it stays out of the infra-bug revert-rate.
    pub fn dropped(token: Pubkey, at_millis: u64) -> Self {
        Self {
            kind: TxKind::Dropped,
            token,
            gross_lamports: 0,
            prio: 0,
            base: 0,
            tip_paid: 0,
            burned_lamports: 0,
            at_millis,
        }
    }

    /// Realized economic net (base-asset lamports): a land nets its gross MINUS its own
    /// priority+base+tip (plan §10 — `gross_lamports` is already net of AMM swap fees, being the
    /// base-ATA balance delta); a revert/drop nets the negative burn.
    pub fn net_lamports(&self) -> i64 {
        match self.kind {
            TxKind::Landed => {
                self.gross_lamports - self.prio as i64 - self.base as i64 - self.tip_paid as i64
            }
            TxKind::Reverted | TxKind::Dropped => -(self.burned_lamports as i64),
        }
    }
}

/// Invariant violation rejected by `record_outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PnlError {
    /// A reverted tx claimed a paid tip (impossible: tip is inside the atomic tx).
    TipPaidOnRevert,
    /// A reverted tx's burned lamports were neither 0 nor exactly base+priority.
    BurnMismatch { burned: u64, expected: u64 },
}

#[derive(Debug, Default)]
pub struct PnlLedger {
    ring: Mutex<VecDeque<TxOutcome>>,
    /// Lifetime realized net = Σ net_lamports() (lands net of own costs, reverts/drops as −burn).
    lifetime_net: AtomicI64,
    lands: AtomicU64,
    reverts: AtomicU64,
    drops: AtomicU64,
}

impl PnlLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an outcome after checking the §9 invariants.
    pub fn record_outcome(&self, outcome: TxOutcome) -> Result<(), PnlError> {
        if matches!(outcome.kind, TxKind::Reverted | TxKind::Dropped) {
            if outcome.tip_paid != 0 {
                return Err(PnlError::TipPaidOnRevert);
            }
            let expected = outcome.prio.saturating_add(outcome.base);
            if outcome.burned_lamports != 0 && outcome.burned_lamports != expected {
                return Err(PnlError::BurnMismatch {
                    burned: outcome.burned_lamports,
                    expected,
                });
            }
        }
        self.lifetime_net
            .fetch_add(outcome.net_lamports(), Ordering::Relaxed);
        match outcome.kind {
            TxKind::Landed => {
                self.lands.fetch_add(1, Ordering::Relaxed);
            }
            TxKind::Reverted => {
                self.reverts.fetch_add(1, Ordering::Relaxed);
            }
            TxKind::Dropped => {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut ring = self.ring.lock().unwrap();
        if ring.len() == RING_CAP {
            ring.pop_front();
        }
        ring.push_back(outcome);
        Ok(())
    }

    /// Lifetime realized PnL = Σ net_lamports() (each land net of its own priority+base+tip).
    pub fn realized_pnl_lifetime(&self) -> i64 {
        self.lifetime_net.load(Ordering::Relaxed)
    }

    /// Burned lamports (reverted losers) within the last `window_millis`.
    pub fn burn_lamports_window(&self, window_millis: u64, now_millis: u64) -> u64 {
        let cutoff = now_millis.saturating_sub(window_millis);
        self.ring
            .lock()
            .unwrap()
            .iter()
            .filter(|o| o.at_millis >= cutoff)
            .map(|o| o.burned_lamports)
            .sum()
    }

    /// Burn-rate in lamports/minute over the window.
    pub fn burn_rate_lamports_per_min(&self, window_millis: u64, now_millis: u64) -> u64 {
        if window_millis == 0 {
            return 0;
        }
        let burned = self.burn_lamports_window(window_millis, now_millis) as u128;
        ((burned * 60_000) / window_millis as u128) as u64
    }

    /// Realized PnL within the window = Σ net_lamports() (lands net of their own costs; reverts/
    /// drops as −burn).
    pub fn realized_pnl_window(&self, window_millis: u64, now_millis: u64) -> i64 {
        let cutoff = now_millis.saturating_sub(window_millis);
        self.ring
            .lock()
            .unwrap()
            .iter()
            .filter(|o| o.at_millis >= cutoff)
            .map(|o| o.net_lamports())
            .sum()
    }

    /// Realized loss in SOL/hour over the window (0 if net positive).
    pub fn realized_loss_sol_per_hour(&self, window_millis: u64, now_millis: u64) -> f64 {
        if window_millis == 0 {
            return 0.0;
        }
        let net = self.realized_pnl_window(window_millis, now_millis);
        if net >= 0 {
            return 0.0;
        }
        let loss_lamports = (-net) as f64;
        let per_ms = loss_lamports / window_millis as f64;
        per_ms * 3_600_000.0 / 1_000_000_000.0
    }

    /// (lands, reverts) within the window — feeds the windowed revert-rate.
    pub fn counts_window(&self, window_millis: u64, now_millis: u64) -> (u64, u64) {
        let cutoff = now_millis.saturating_sub(window_millis);
        let ring = self.ring.lock().unwrap();
        let mut lands = 0u64;
        let mut reverts = 0u64;
        for o in ring.iter().filter(|o| o.at_millis >= cutoff) {
            match o.kind {
                TxKind::Landed => lands += 1,
                TxKind::Reverted => reverts += 1,
                // Pre-inclusion drops are NORMAL competitive losses (zero cost), not an infra signal
                // — excluded from the >30% infra-bug revert-rate numerator AND denominator (plan §6).
                TxKind::Dropped => {}
            }
        }
        (lands, reverts)
    }

    /// Windowed revert-rate percent = reverts / (lands + reverts) · 100.
    pub fn revert_rate_pct_window(&self, window_millis: u64, now_millis: u64) -> f64 {
        let (lands, reverts) = self.counts_window(window_millis, now_millis);
        let resolved = lands + reverts;
        if resolved == 0 {
            0.0
        } else {
            reverts as f64 * 100.0 / resolved as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Pubkey {
        Pubkey::new_from_array([9; 32])
    }

    #[test]
    fn rejects_reverted_with_tip_paid() {
        let led = PnlLedger::new();
        let bad = TxOutcome {
            kind: TxKind::Reverted,
            token: token(),
            gross_lamports: 0,
            prio: 1_000,
            base: 5_000,
            tip_paid: 10, // illegal
            burned_lamports: 6_000,
            at_millis: 0,
        };
        assert_eq!(led.record_outcome(bad), Err(PnlError::TipPaidOnRevert));
    }

    #[test]
    fn rejects_burn_mismatch() {
        let led = PnlLedger::new();
        let bad = TxOutcome {
            kind: TxKind::Reverted,
            token: token(),
            gross_lamports: 0,
            prio: 1_000,
            base: 5_000,
            tip_paid: 0,
            burned_lamports: 9_999, // != prio+base and != 0
            at_millis: 0,
        };
        assert_eq!(
            led.record_outcome(bad),
            Err(PnlError::BurnMismatch {
                burned: 9_999,
                expected: 6_000
            })
        );
    }

    #[test]
    fn burn_window_matches_manual_sum() {
        let led = PnlLedger::new();
        // now = 100_000 ms; 60s window keeps at_millis >= 40_000.
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1_000, 5_000, 30_000))
            .unwrap(); // outside window
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1_000, 5_000, 50_000))
            .unwrap(); // inside
        led.record_outcome(TxOutcome::reverted_onchain(token(), 2_000, 5_000, 90_000))
            .unwrap(); // inside
        let burned = led.burn_lamports_window(60_000, 100_000);
        assert_eq!(burned, 6_000 + 7_000);
        // 13_000 lamports over 60s => 13_000 * 60_000 / 60_000 = 13_000 lamports/min.
        assert_eq!(led.burn_rate_lamports_per_min(60_000, 100_000), 13_000);
    }

    #[test]
    fn realized_pnl_nets_landed_costs_and_burned() {
        let led = PnlLedger::new();
        led.record_outcome(TxOutcome::landed(token(), 100_000, 1_000, 5_000, 2_000, 10))
            .unwrap();
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1_000, 5_000, 20))
            .unwrap();
        // land net = 100_000 - 1_000(prio) - 5_000(base) - 2_000(tip) = 92_000 ; revert burn = -6_000.
        assert_eq!(led.realized_pnl_lifetime(), 86_000);
        assert_eq!(led.realized_pnl_window(1_000, 1_000), 86_000);
    }

    #[test]
    fn fee_heavy_marginal_lands_net_negative_so_loss_killswitch_can_trip() {
        let led = PnlLedger::new();
        // 100 lands each gross 1_000 but prio 4_000 + base 5_000 + tip 2_000 = true net −10_000/tx.
        for i in 0..100u64 {
            led.record_outcome(TxOutcome::landed(token(), 1_000, 4_000, 5_000, 2_000, i))
                .unwrap();
        }
        assert_eq!(led.realized_pnl_lifetime(), -1_000_000);
        assert!(led.realized_loss_sol_per_hour(1_000_000, 1_000_000) > 0.0);
    }

    #[test]
    fn drops_do_not_inflate_the_revert_rate() {
        let led = PnlLedger::new();
        led.record_outcome(TxOutcome::landed(token(), 10, 0, 0, 0, 1_000))
            .unwrap();
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1, 1, 1_000))
            .unwrap();
        // 40 pre-inclusion auction-loss drops (normal competitive losses, zero cost).
        for _ in 0..40 {
            led.record_outcome(TxOutcome::dropped(token(), 1_000))
                .unwrap();
        }
        // Revert-rate is over block-reaching txs only: 1 revert / (1 land + 1 revert) = 50%, NOT
        // 41/42 — the drops are excluded.
        let pct = led.revert_rate_pct_window(10_000, 1_000);
        assert!((pct - 50.0).abs() < 1e-9, "pct={pct}");
        // Drops burn nothing.
        assert_eq!(led.burn_lamports_window(10_000, 1_000), 2);
    }

    #[test]
    fn windowed_revert_rate() {
        let led = PnlLedger::new();
        led.record_outcome(TxOutcome::landed(token(), 10, 0, 0, 0, 1_000))
            .unwrap();
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1, 1, 1_000))
            .unwrap();
        led.record_outcome(TxOutcome::reverted_onchain(token(), 1, 1, 1_000))
            .unwrap();
        // 2 reverts of 3 resolved => 66.67%.
        let pct = led.revert_rate_pct_window(10_000, 1_000);
        assert!((pct - 66.666).abs() < 0.01, "pct={pct}");
    }
}
