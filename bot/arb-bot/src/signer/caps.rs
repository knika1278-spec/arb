//! signer-4 — synchronous, no-I/O pre-sign caps (count + cumulative lamport-out).
//!
//! A token-bucket the sidecar checks while holding the sign mutex, BEFORE touching the key, so the
//! worst-case outflow per interval is bounded *before* lagging revert-rate / loss metrics catch up.
//! `reserve` performs zero syscalls — time is a caller-supplied logical millisecond clock so the
//! bucket is deterministic and allocation-free.
//!
//! **dec-2 (CapReservation lifecycle across the landing rebuild loop):** a rebuild-resign of the
//! SAME opportunity must consume exactly ONE count slot. The landing loop achieves this by
//! `release`-ing the prior reservation before reserving anew within the same window epoch
//! (restoring both count and lamport budget), or by carrying one reservation handle across
//! rebuilds. `release` is a no-op once the window has rolled (the budget already reset).

/// The lamport-out budget is seeded from a balance snapshot pushed by the sweeper/health loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceSnapshot {
    /// hot balance − working_reserve − rent-exempt minimum.
    pub spendable_lamports: u64,
}

/// A successful reservation; carry it to `release` it on a dropped/rebuilt tx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapReservation {
    pub lamport_out: u64,
    /// The window epoch the reservation was charged against (release is a no-op in a later epoch).
    pub window_epoch: u64,
}

/// Why a reservation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapExceeded {
    /// The per-interval signature count ceiling was hit.
    Count { max: u32 },
    /// The cumulative lamport-out budget would be exceeded.
    Lamport {
        requested: u64,
        used: u64,
        budget: u64,
    },
}

/// The synchronous local rate/outflow ceiling.
#[derive(Clone, Copy, Debug)]
pub struct PreSignCaps {
    interval_millis: u64,
    max_sigs_per_interval: u32,
    config_lamport_cap: u64,
    // mutable window state
    window_start_millis: u64,
    window_epoch: u64,
    sigs_in_window: u32,
    lamport_out_budget: u64,
    lamport_out_used: u64,
}

impl PreSignCaps {
    /// `config_lamport_cap` is the hard per-window outflow ceiling; the live budget is the lesser
    /// of it and the latest spendable-balance snapshot.
    pub fn new(interval_millis: u64, max_sigs_per_interval: u32, config_lamport_cap: u64) -> Self {
        Self {
            interval_millis,
            max_sigs_per_interval,
            config_lamport_cap,
            window_start_millis: 0,
            window_epoch: 0,
            sigs_in_window: 0,
            lamport_out_budget: config_lamport_cap,
            lamport_out_used: 0,
        }
    }

    /// Reseed `lamport_out_budget = min(config_cap, spendable)` (pushed by sweeper/health loop).
    pub fn apply_snapshot(&mut self, s: BalanceSnapshot) {
        self.lamport_out_budget = self.config_lamport_cap.min(s.spendable_lamports);
        if self.lamport_out_used > self.lamport_out_budget {
            // Tightened budget below what is already used this window: clamp (no negative).
            self.lamport_out_used = self.lamport_out_budget;
        }
    }

    fn roll_window(&mut self, now_millis: u64) {
        if now_millis.saturating_sub(self.window_start_millis) >= self.interval_millis {
            self.window_start_millis = now_millis;
            self.window_epoch += 1;
            self.sigs_in_window = 0;
            self.lamport_out_used = 0;
        }
    }

    /// SYNCHRONOUS, no-I/O. Rolls the window, checks both ceilings, charges the reservation.
    pub fn reserve(
        &mut self,
        lamport_out: u64,
        now_millis: u64,
    ) -> Result<CapReservation, CapExceeded> {
        self.roll_window(now_millis);
        if self.sigs_in_window + 1 > self.max_sigs_per_interval {
            return Err(CapExceeded::Count {
                max: self.max_sigs_per_interval,
            });
        }
        let new_used = self.lamport_out_used.saturating_add(lamport_out);
        if new_used > self.lamport_out_budget {
            return Err(CapExceeded::Lamport {
                requested: lamport_out,
                used: self.lamport_out_used,
                budget: self.lamport_out_budget,
            });
        }
        self.sigs_in_window += 1;
        self.lamport_out_used = new_used;
        Ok(CapReservation {
            lamport_out,
            window_epoch: self.window_epoch,
        })
    }

    /// Restore a reservation (dropped/rebuilt tx). No-op if the window has since rolled.
    pub fn release(&mut self, res: CapReservation, now_millis: u64) {
        self.roll_window(now_millis);
        if res.window_epoch == self.window_epoch {
            self.sigs_in_window = self.sigs_in_window.saturating_sub(1);
            self.lamport_out_used = self.lamport_out_used.saturating_sub(res.lamport_out);
        }
    }

    pub fn sigs_in_window(&self) -> u32 {
        self.sigs_in_window
    }
    pub fn lamport_out_used(&self) -> u64 {
        self.lamport_out_used
    }
    pub fn lamport_out_budget(&self) -> u64 {
        self.lamport_out_budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_cap_blocks_n_plus_first_in_window() {
        let mut caps = PreSignCaps::new(60_000, 3, u64::MAX);
        for _ in 0..3 {
            caps.reserve(0, 1_000).unwrap();
        }
        assert_eq!(caps.reserve(0, 1_000), Err(CapExceeded::Count { max: 3 }));
        // Next window resets the count.
        assert!(caps.reserve(0, 61_001).is_ok());
    }

    #[test]
    fn lamport_budget_blocks_over_cap_reservation() {
        let mut caps = PreSignCaps::new(60_000, 100, 10_000);
        caps.reserve(7_000, 0).unwrap();
        assert_eq!(
            caps.reserve(4_000, 0),
            Err(CapExceeded::Lamport {
                requested: 4_000,
                used: 7_000,
                budget: 10_000
            })
        );
        // Exactly hitting the budget is allowed.
        assert!(caps.reserve(3_000, 0).is_ok());
        assert_eq!(caps.lamport_out_used(), 10_000);
    }

    #[test]
    fn release_restores_count_and_lamport_within_window() {
        let mut caps = PreSignCaps::new(60_000, 2, 10_000);
        let r = caps.reserve(5_000, 0).unwrap();
        assert_eq!(caps.sigs_in_window(), 1);
        assert_eq!(caps.lamport_out_used(), 5_000);
        caps.release(r, 0);
        assert_eq!(caps.sigs_in_window(), 0);
        assert_eq!(caps.lamport_out_used(), 0);
    }

    #[test]
    fn dec2_n_rebuilds_consume_one_count_slot() {
        // The landing loop rebuilds the SAME opportunity N times on no-land, release-before-reserve
        // each time => exactly one count slot is held at any moment, and the per-window count after
        // N rebuilds is 1 (not N).
        let mut caps = PreSignCaps::new(60_000, 1, u64::MAX); // max ONE sig per window
        let mut res = caps.reserve(1_000, 0).unwrap();
        for i in 1..=5u64 {
            // rebuild #i: release the prior reservation, then reserve anew — same window epoch.
            caps.release(res, i * 100);
            res = caps
                .reserve(1_000, i * 100)
                .expect("release-before-reserve keeps us within the 1-sig cap");
        }
        assert_eq!(caps.sigs_in_window(), 1);
    }

    #[test]
    fn apply_snapshot_tightens_budget_to_spendable() {
        let mut caps = PreSignCaps::new(60_000, 100, 1_000_000);
        caps.apply_snapshot(BalanceSnapshot {
            spendable_lamports: 50_000,
        });
        assert_eq!(caps.lamport_out_budget(), 50_000);
        assert_eq!(
            caps.reserve(60_000, 0),
            Err(CapExceeded::Lamport {
                requested: 60_000,
                used: 0,
                budget: 50_000
            })
        );
    }
}
