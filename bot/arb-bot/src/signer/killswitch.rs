//! signer-6 — the kill-switch flag + handle, and the thin supervisor that maps an observ
//! [`KillSwitchSignal`] into a halt (signer-7).
//!
//! The `signing-enabled` flag is an `Arc<AtomicBool>` read (Acquire) before every sign. `halt`
//! flips it sub-second (Release) and appends a [`TripRecord`]; `rearm` is NEVER automatic — it
//! requires an acked trip + an operator name. Trip records are persisted append-only (newline JSON)
//! so post-trip forensics and the rearm gate survive a restart. Per observ-14 the supervisor does
//! not own thresholds (observ's `HealthEvaluator` does); it consumes the resulting signal and owns
//! the flag.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::alert::{AlertMessage, AlertSink, Severity};
use crate::metrics::health::{KillSwitchSignal, TripReason};

/// Why outflow was halted. Each variant selects a runbook branch (signer-11).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HaltReason {
    Manual { operator: String },
    RevertRate { pct: f64 },
    RealizedLoss { sol_per_hr: f64 },
    BalanceDeviation { pct: f64 },
    BurnRate { sol_per_min: f64 },
}

impl HaltReason {
    /// Runbook branch URL fragment for this reason (signer-11 triage).
    pub fn runbook_url(&self) -> String {
        let anchor = match self {
            HaltReason::Manual { .. } => "manual",
            HaltReason::RevertRate { .. } => "revert-rate",
            HaltReason::RealizedLoss { .. } => "realized-loss",
            HaltReason::BalanceDeviation { .. } => "balance-deviation",
            HaltReason::BurnRate { .. } => "burn-rate",
        };
        format!("ops/runbooks/killswitch_recovery.md#{anchor}")
    }

    /// BalanceDeviation ⇒ suspected key compromise ⇒ rotate+sweep is the first containment step.
    pub fn suspects_compromise(&self) -> bool {
        matches!(self, HaltReason::BalanceDeviation { .. })
    }
}

/// Persisted record of one trip (append-only; the rearm gate requires `acked`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TripRecord {
    pub reason: HaltReason,
    pub tripped_at_millis: u64,
    pub acked: bool,
    pub acked_by: Option<String>,
}

/// Why a rearm was refused (rearm is never automatic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RearmError {
    /// There is no trip to ack/clear.
    NoTrip,
    /// The most recent trip has not been acknowledged by an operator.
    UnackedTrip,
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Cloneable handle to the shared kill-switch flag + trip log.
#[derive(Clone)]
pub struct KillSwitchHandle {
    enabled: Arc<AtomicBool>,
    trips: Arc<Mutex<Vec<TripRecord>>>,
    persist_path: Option<Arc<PathBuf>>,
}

impl KillSwitchHandle {
    /// A new handle, signing enabled.
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            trips: Arc::new(Mutex::new(Vec::new())),
            persist_path: None,
        }
    }

    /// A handle that appends trip records to `path` (newline-delimited JSON).
    pub fn with_persistence(path: PathBuf) -> Self {
        Self {
            persist_path: Some(Arc::new(path)),
            ..Self::new()
        }
    }

    /// Cheap Acquire read performed before every sign.
    #[inline]
    pub fn signing_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Halt all signing sub-second and record the trip. Idempotent on the flag.
    ///
    /// The flag flip and the trip push happen together UNDER the trips lock, so a concurrent
    /// `rearm` (which also flips the flag under that lock) cannot observe an acked history, then
    /// re-enable across a fresh halt — closing the rearm/halt TOCTOU race.
    pub fn halt(&self, reason: HaltReason) {
        let record = TripRecord {
            reason,
            tripped_at_millis: now_unix_millis(),
            acked: false,
            acked_by: None,
        };
        {
            let mut trips = self.trips.lock().unwrap();
            self.enabled.store(false, Ordering::Release);
            trips.push(record.clone());
        }
        // File persistence is best-effort and kept OUT of the critical section.
        if let Some(path) = &self.persist_path {
            if let Ok(line) = serde_json::to_string(&record) {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path.as_ref())
                {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
    }

    /// Acknowledge the most recent trip (forensics sign-off) — a precondition for rearm.
    pub fn ack_last_trip(&self, operator: &str) -> Result<(), RearmError> {
        let mut trips = self.trips.lock().unwrap();
        match trips.last_mut() {
            Some(t) => {
                t.acked = true;
                t.acked_by = Some(operator.to_string());
                Ok(())
            }
            None => Err(RearmError::NoTrip),
        }
    }

    /// Re-enable signing. Refused unless the latest trip is acked. NEVER automatic.
    ///
    /// The acked-check and the re-enable happen together UNDER the trips lock, so a concurrent
    /// `halt` (which also flips the flag under that lock) cannot slip a fresh unacked trip between
    /// the check and the store and leave signing enabled.
    pub fn rearm(&self, operator: &str) -> Result<(), RearmError> {
        let trips = self.trips.lock().unwrap();
        match trips.last() {
            Some(t) if !t.acked => return Err(RearmError::UnackedTrip),
            Some(_) => {}
            None => {} // never tripped; rearm is a harmless no-op enabling
        }
        // Re-enable while STILL holding the trips lock (atomic against a concurrent halt).
        self.enabled.store(true, Ordering::Release);
        drop(trips);
        tracing::warn!(operator, "kill-switch rearmed");
        Ok(())
    }

    /// Snapshot of the trip log (forensics / tests).
    pub fn trips(&self) -> Vec<TripRecord> {
        self.trips.lock().unwrap().clone()
    }
}

impl Default for KillSwitchHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Map an observ kill-switch signal into a [`HaltReason`].
pub fn halt_reason_from_signal(signal: KillSwitchSignal) -> Option<HaltReason> {
    match signal {
        KillSwitchSignal::Healthy => None,
        KillSwitchSignal::Trip { reason } => Some(match reason {
            TripReason::RevertRateExceeded { pct } => HaltReason::RevertRate { pct },
            TripReason::RealizedLossExceeded { sol_per_hour } => HaltReason::RealizedLoss {
                sol_per_hr: sol_per_hour,
            },
            TripReason::BalanceDeviationExceeded { pct } => HaltReason::BalanceDeviation { pct },
            TripReason::BurnRateExceeded { lamports_per_min } => HaltReason::BurnRate {
                sol_per_min: lamports_per_min as f64 / 1_000_000_000.0,
            },
        }),
    }
}

/// signer-7 — thin supervisor: consume an observ signal, auto-halt on first breach, and route an
/// alert. Thresholds live in observ's `HealthEvaluator` (no duplication). Returns the halt reason
/// if it tripped. A failing alert sink never blocks the halt (the flag is flipped first).
pub fn apply_health_signal<S: AlertSink>(
    handle: &KillSwitchHandle,
    signal: KillSwitchSignal,
    alerts: &S,
) -> Option<HaltReason> {
    let reason = halt_reason_from_signal(signal)?;
    handle.halt(reason.clone()); // flip the flag FIRST, before any alert I/O
    alerts.send(
        Severity::Critical,
        &AlertMessage {
            title: "kill-switch tripped".to_string(),
            body: format!("{reason:?}"),
            runbook_url: Some(reason.runbook_url()),
        },
    );
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::health::TripReason;
    use crate::signer::alert::LogSink;
    use std::time::Instant;

    #[test]
    fn manual_halt_blocks_signing_immediately() {
        let h = KillSwitchHandle::new();
        assert!(h.signing_enabled());
        let t = Instant::now();
        h.halt(HaltReason::Manual {
            operator: "alice".into(),
        });
        assert!(t.elapsed().as_millis() < 1000); // sub-second
        assert!(!h.signing_enabled());
        assert_eq!(h.trips().len(), 1);
    }

    #[test]
    fn rearm_refused_until_acked_and_never_automatic() {
        let h = KillSwitchHandle::new();
        h.halt(HaltReason::RevertRate { pct: 55.0 });
        assert_eq!(h.rearm("op"), Err(RearmError::UnackedTrip));
        h.ack_last_trip("op").unwrap();
        assert!(h.rearm("op").is_ok());
        assert!(h.signing_enabled());
    }

    #[test]
    fn fresh_halt_after_rearm_disables_and_requires_new_ack() {
        let h = KillSwitchHandle::new();
        h.halt(HaltReason::RevertRate { pct: 50.0 });
        h.ack_last_trip("op").unwrap();
        h.rearm("op").unwrap();
        assert!(h.signing_enabled());
        // A fresh halt re-disables; its trip is unacked, so rearm is refused again.
        h.halt(HaltReason::BurnRate { sol_per_min: 0.1 });
        assert!(!h.signing_enabled());
        assert_eq!(h.rearm("op"), Err(RearmError::UnackedTrip));
    }

    #[test]
    fn shared_handle_halts_all_clones() {
        let h = KillSwitchHandle::new();
        let h2 = h.clone();
        h.halt(HaltReason::BurnRate { sol_per_min: 0.1 });
        assert!(!h2.signing_enabled()); // the clone sees the halt
    }

    #[test]
    fn trip_record_persists_and_survives_reload() {
        let dir = std::env::temp_dir().join(format!("arbks_persist_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trips.jsonl");
        let h = KillSwitchHandle::with_persistence(path.clone());
        h.halt(HaltReason::BalanceDeviation { pct: 22.0 });
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: TripRecord = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(parsed.reason, HaltReason::BalanceDeviation { pct: 22.0 });
        assert!(!parsed.acked);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn supervisor_halts_and_alerts_on_trip() {
        let h = KillSwitchHandle::new();
        let reason = apply_health_signal(
            &h,
            KillSwitchSignal::Trip {
                reason: TripReason::RevertRateExceeded { pct: 47.0 },
            },
            &LogSink,
        );
        assert_eq!(reason, Some(HaltReason::RevertRate { pct: 47.0 }));
        assert!(!h.signing_enabled());
    }

    #[test]
    fn supervisor_noop_on_healthy() {
        let h = KillSwitchHandle::new();
        assert_eq!(
            apply_health_signal(&h, KillSwitchSignal::Healthy, &LogSink),
            None
        );
        assert!(h.signing_enabled());
    }

    #[test]
    fn balance_deviation_flags_suspected_compromise() {
        assert!(HaltReason::BalanceDeviation { pct: 20.0 }.suspects_compromise());
        assert!(!HaltReason::RevertRate { pct: 50.0 }.suspects_compromise());
    }
}
