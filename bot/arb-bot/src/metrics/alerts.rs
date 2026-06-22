//! observ-8 — deviation-alert router. Takes a [`KillSwitchSignal`] and dispatches a deduplicated,
//! runbook-linked alert to an [`AlertSink`] (off hot path). Per-reason dedup suppresses a storm of
//! identical trips within a window; each alert carries the per-reason runbook URL. The networked
//! Telegram/PagerDuty sinks implement [`AlertSink`] (reqwest) in their phase; a failing sink can
//! never block the evaluator because [`AlertSink::send`] is fire-and-forget by contract.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::metrics::health::{KillSwitchSignal, TripReason};
use crate::signer::alert::{AlertMessage, AlertSink, Severity};

/// Stable per-reason key for dedup + the runbook anchor.
fn reason_key(r: &TripReason) -> &'static str {
    match r {
        TripReason::RevertRateExceeded { .. } => "revert-rate",
        TripReason::BurnRateExceeded { .. } => "burn-rate",
        TripReason::RealizedLossExceeded { .. } => "realized-loss",
        TripReason::BalanceDeviationExceeded { .. } => "balance-deviation",
    }
}

fn runbook_url(key: &str) -> String {
    format!("ops/runbooks/killswitch_recovery.md#{key}")
}

/// Routes trip/deviation signals to a sink with per-reason dedup.
pub struct AlertRouter<S: AlertSink> {
    sink: S,
    dedup_window_millis: u64,
    last_sent: Mutex<HashMap<&'static str, u64>>,
}

impl<S: AlertSink> AlertRouter<S> {
    pub fn new(sink: S, dedup_window_millis: u64) -> Self {
        Self {
            sink,
            dedup_window_millis,
            last_sent: Mutex::new(HashMap::new()),
        }
    }

    /// Dispatch a signal. Returns `true` if an alert was actually sent (not deduped / not healthy).
    pub fn dispatch(&self, signal: KillSwitchSignal, now_millis: u64) -> bool {
        let reason = match signal {
            KillSwitchSignal::Healthy => return false,
            KillSwitchSignal::Trip { reason } => reason,
        };
        let key = reason_key(&reason);
        {
            let mut last = self.last_sent.lock().unwrap();
            if let Some(&prev) = last.get(key) {
                if now_millis.saturating_sub(prev) < self.dedup_window_millis {
                    return false; // within dedup window — suppress
                }
            }
            last.insert(key, now_millis);
        }
        self.sink.send(
            Severity::Critical,
            &AlertMessage {
                title: format!("kill-switch: {key}"),
                body: format!("{reason:?}"),
                runbook_url: Some(runbook_url(key)),
            },
        );
        true
    }

    /// Borrow the sink (e.g. a recording test sink).
    pub fn sink(&self) -> &S {
        &self.sink
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::alert::AlertMessage;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Recording {
        seen: StdMutex<Vec<AlertMessage>>,
    }
    impl AlertSink for Recording {
        fn send(&self, _sev: Severity, msg: &AlertMessage) {
            self.seen.lock().unwrap().push(msg.clone());
        }
    }

    fn trip(pct: f64) -> KillSwitchSignal {
        KillSwitchSignal::Trip {
            reason: TripReason::RevertRateExceeded { pct },
        }
    }

    #[test]
    fn healthy_dispatches_nothing() {
        let r = AlertRouter::new(Recording::default(), 60_000);
        assert!(!r.dispatch(KillSwitchSignal::Healthy, 0));
        assert!(r.sink().seen.lock().unwrap().is_empty());
    }

    #[test]
    fn one_alert_per_dedup_window_per_reason() {
        let r = AlertRouter::new(Recording::default(), 60_000);
        assert!(r.dispatch(trip(50.0), 0)); // sent
        assert!(!r.dispatch(trip(55.0), 30_000)); // within window => deduped
        assert!(r.dispatch(trip(60.0), 70_000)); // window elapsed => sent again
        assert_eq!(r.sink().seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn distinct_reasons_are_not_deduped_against_each_other() {
        let r = AlertRouter::new(Recording::default(), 60_000);
        assert!(r.dispatch(trip(50.0), 0));
        assert!(r.dispatch(
            KillSwitchSignal::Trip {
                reason: TripReason::BurnRateExceeded {
                    lamports_per_min: 99_999
                }
            },
            10
        ));
        assert_eq!(r.sink().seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn alert_carries_runbook_url() {
        let r = AlertRouter::new(Recording::default(), 60_000);
        r.dispatch(trip(50.0), 0);
        let seen = r.sink().seen.lock().unwrap();
        assert_eq!(
            seen[0].runbook_url.as_deref(),
            Some("ops/runbooks/killswitch_recovery.md#revert-rate")
        );
    }
}
