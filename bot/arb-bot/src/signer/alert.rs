//! Alert sink seam (part of signer-7). Halt trips and sweep anomalies fan out to an [`AlertSink`].
//! The networked Telegram/PagerDuty sinks (reqwest+rustls) are a Fase-2 implementation behind this
//! trait; M1 ships the [`LogSink`] fallback so a failing alert sink can never block a halt.

/// Alert severity for routing/dedup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A single alert payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertMessage {
    pub title: String,
    pub body: String,
    /// Per-reason runbook URL (observ-8 requirement: every trip carries a runbook link).
    pub runbook_url: Option<String>,
}

/// Fire-and-forget alert delivery. Implementations MUST NOT panic or block the caller (a failing
/// sink logs and returns; it never propagates into the halt path).
pub trait AlertSink: Send + Sync {
    fn send(&self, sev: Severity, msg: &AlertMessage);
}

/// The M1 fallback sink: structured tracing logs. Always available, never blocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogSink;

impl AlertSink for LogSink {
    fn send(&self, sev: Severity, msg: &AlertMessage) {
        match sev {
            Severity::Critical => {
                tracing::error!(title = %msg.title, body = %msg.body, runbook = ?msg.runbook_url, "ALERT")
            }
            Severity::Warning => {
                tracing::warn!(title = %msg.title, body = %msg.body, runbook = ?msg.runbook_url, "ALERT")
            }
            Severity::Info => {
                tracing::info!(title = %msg.title, body = %msg.body, runbook = ?msg.runbook_url, "ALERT")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A test sink that records what it received (proves dispatch happened without a network call).
    #[derive(Default)]
    pub struct RecordingSink {
        pub seen: Mutex<Vec<(Severity, AlertMessage)>>,
    }

    impl AlertSink for RecordingSink {
        fn send(&self, sev: Severity, msg: &AlertMessage) {
            self.seen.lock().unwrap().push((sev, msg.clone()));
        }
    }

    #[test]
    fn log_sink_does_not_panic() {
        let s = LogSink;
        s.send(
            Severity::Critical,
            &AlertMessage {
                title: "halt".into(),
                body: "revert-rate".into(),
                runbook_url: Some("https://runbook/killswitch".into()),
            },
        );
    }

    #[test]
    fn recording_sink_captures_dispatch() {
        let s = RecordingSink::default();
        s.send(
            Severity::Warning,
            &AlertMessage {
                title: "t".into(),
                body: "b".into(),
                runbook_url: None,
            },
        );
        assert_eq!(s.seen.lock().unwrap().len(), 1);
    }
}
