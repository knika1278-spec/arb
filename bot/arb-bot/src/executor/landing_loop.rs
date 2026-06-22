//! landing-6 — the strict landing loop, and landing-10 — the `BlockhashSource` seam.
//!
//! The loop submits an attempt, and on no-land REBUILDS with a FRESH blockhash and resubmits, never
//! reusing a blockhash, bounded by `max_attempts`. Each failed attempt is attributed a best-effort
//! [`DropCause`]. The networked submit→poll→status machinery (JitoClient/HeliusSender/RpcClient) is
//! abstracted behind [`LandingTransport`]; the blockhash source is abstracted behind
//! [`BlockhashSource`] (landing-10) so a durable-nonce evaluation can replace fresh-blockhash
//! rebuild later with no restructuring — the durable-nonce variant compiles but is disabled for M1.

use std::collections::HashSet;

use super::types::{ArbTxSpec, Blockhash, DropCause, LandingOutcome, Route};

/// Source of recent blockhashes (landing-10 seam). The real impl fetches via RPC; the durable-nonce
/// impl is feature-flagged OFF for M1.
pub trait BlockhashSource {
    /// A fresh recent blockhash for the next attempt.
    fn fresh(&self) -> Result<Blockhash, DropCause>;
    /// Whether this source is the (disabled-for-M1) durable-nonce path.
    fn is_durable_nonce(&self) -> bool {
        false
    }
}

/// Outcome of one submit→poll attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptResult {
    Landed { slot: u64 },
    Reverted { slot: u64, burned_lamports: u64 },
    NoLand { cause: DropCause },
}

/// The submit→poll→status transport (JitoClient / HeliusSender / SWQoS behind one seam).
pub trait LandingTransport {
    fn attempt(&self, spec: &ArbTxSpec, blockhash: Blockhash, attempt: u8) -> AttemptResult;
}

/// Run the submit→(rebuild on no-land)→retry loop. Returns the terminal outcome.
pub fn run_landing_loop(
    source: &dyn BlockhashSource,
    transport: &dyn LandingTransport,
    spec: &ArbTxSpec,
    route: Route,
    tip_paid_lamports: u64,
    max_attempts: u8,
) -> LandingOutcome {
    let mut used: HashSet<[u8; 32]> = HashSet::new();
    let mut last_cause = DropCause::Unknown;
    // Count ACTUAL transport submissions, not loop iterations — a blockhash-fetch error or a
    // duplicate-hash `continue` never reaches the transport, so it must not inflate `attempts`.
    let mut submitted: u8 = 0;

    for _ in 0..max_attempts {
        let bh = match source.fresh() {
            Ok(b) => b,
            Err(cause) => {
                last_cause = cause;
                continue;
            }
        };
        // Never reuse a blockhash across attempts; a repeat from the source is treated as stale.
        if !used.insert(bh.0) {
            last_cause = DropCause::StaleBlockhash;
            continue;
        }
        submitted += 1;
        match transport.attempt(spec, bh, submitted) {
            AttemptResult::Landed { slot } => {
                return LandingOutcome::Landed {
                    slot,
                    attempts: submitted,
                    tip_paid_lamports,
                    route,
                    latency_ms: 0, // populated from latency spans at the real seam
                };
            }
            AttemptResult::Reverted {
                slot,
                burned_lamports,
            } => {
                return LandingOutcome::Reverted {
                    slot,
                    attempts: submitted,
                    burned_lamports,
                };
            }
            AttemptResult::NoLand { cause } => {
                last_cause = cause; // rebuild with a fresh blockhash on the next iteration
            }
        }
    }

    LandingOutcome::GaveUp {
        attempts: submitted,
        last_cause,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// Hands out a fresh, distinct blockhash on every call.
    #[derive(Default)]
    struct IncrementingBlockhashSource {
        n: AtomicU8,
    }
    impl BlockhashSource for IncrementingBlockhashSource {
        fn fresh(&self) -> Result<Blockhash, DropCause> {
            let i = self.n.fetch_add(1, Ordering::Relaxed);
            Ok(Blockhash([i; 32]))
        }
    }

    /// Records the blockhashes it saw; lands after `land_on` attempts.
    struct RecordingTransport {
        land_on: u8,
        seen: RefCell<Vec<[u8; 32]>>,
    }
    impl LandingTransport for RecordingTransport {
        fn attempt(&self, _spec: &ArbTxSpec, blockhash: Blockhash, attempt: u8) -> AttemptResult {
            self.seen.borrow_mut().push(blockhash.0);
            if attempt >= self.land_on {
                AttemptResult::Landed {
                    slot: 1000 + attempt as u64,
                }
            } else {
                AttemptResult::NoLand {
                    cause: DropCause::TipAuctionLost,
                }
            }
        }
    }

    fn spec() -> ArbTxSpec {
        ArbTxSpec {
            payer: Pubkey::new_from_array([1; 32]),
            cu_limit: 200_000,
            cu_price_micro: 50,
            sim_profit_lamports: 100_000,
            route_pools: vec![Pubkey::new_from_array([9; 32])],
            alt_tables: vec![],
        }
    }

    #[test]
    fn rebuilds_with_distinct_blockhash_each_attempt() {
        let source = IncrementingBlockhashSource::default();
        let transport = RecordingTransport {
            land_on: 3,
            seen: RefCell::new(vec![]),
        };
        let outcome = run_landing_loop(
            &source,
            &transport,
            &spec(),
            Route::JitoBundle {
                region: super::super::types::Region::Frankfurt,
            },
            5_000,
            4,
        );
        assert!(matches!(
            outcome,
            LandingOutcome::Landed { attempts: 3, .. }
        ));
        let seen = transport.seen.borrow();
        // 3 attempts, all distinct blockhashes (no reuse across rebuilds).
        assert_eq!(seen.len(), 3);
        let distinct: HashSet<_> = seen.iter().collect();
        assert_eq!(distinct.len(), 3);
    }

    #[test]
    fn gives_up_after_max_attempts_with_last_cause() {
        let source = IncrementingBlockhashSource::default();
        let transport = RecordingTransport {
            land_on: 99, // never lands
            seen: RefCell::new(vec![]),
        };
        let outcome = run_landing_loop(&source, &transport, &spec(), Route::Swqos, 0, 3);
        assert_eq!(
            outcome,
            LandingOutcome::GaveUp {
                attempts: 3,
                last_cause: DropCause::TipAuctionLost
            }
        );
    }

    #[test]
    fn gaveup_reports_actual_submissions_not_the_budget() {
        // A source that always returns the SAME blockhash: only the first iteration submits; the
        // rest hit the duplicate-hash `continue`. GaveUp.attempts must be 1, not max_attempts.
        struct SameHash;
        impl BlockhashSource for SameHash {
            fn fresh(&self) -> Result<Blockhash, DropCause> {
                Ok(Blockhash([7; 32]))
            }
        }
        struct CountingTransport {
            calls: std::cell::Cell<u8>,
        }
        impl LandingTransport for CountingTransport {
            fn attempt(&self, _s: &ArbTxSpec, _b: Blockhash, _a: u8) -> AttemptResult {
                self.calls.set(self.calls.get() + 1);
                AttemptResult::NoLand {
                    cause: DropCause::TipAuctionLost,
                }
            }
        }
        let t = CountingTransport {
            calls: std::cell::Cell::new(0),
        };
        let outcome = run_landing_loop(&SameHash, &t, &spec(), Route::Swqos, 0, 4);
        assert_eq!(t.calls.get(), 1); // only one real submission
        assert_eq!(
            outcome,
            LandingOutcome::GaveUp {
                attempts: 1,
                last_cause: DropCause::StaleBlockhash
            }
        );
    }

    #[test]
    fn reverted_attempt_terminates_with_burn() {
        struct RevertingTransport;
        impl LandingTransport for RevertingTransport {
            fn attempt(&self, _s: &ArbTxSpec, _b: Blockhash, _a: u8) -> AttemptResult {
                AttemptResult::Reverted {
                    slot: 42,
                    burned_lamports: 6_000,
                }
            }
        }
        let source = IncrementingBlockhashSource::default();
        let outcome = run_landing_loop(&source, &RevertingTransport, &spec(), Route::Swqos, 0, 4);
        assert_eq!(
            outcome,
            LandingOutcome::Reverted {
                slot: 42,
                attempts: 1,
                burned_lamports: 6_000
            }
        );
    }
}
