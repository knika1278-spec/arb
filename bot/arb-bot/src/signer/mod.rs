//! Signer sidecar (signer-2..7): the in-process ed25519 signing sidecar that is the SOLE outflow
//! gate of the bot. It holds ONLY a small-balance hot key ([`keychain::MemorySigner`]) and, before
//! every signature, enforces (1) a kill-switch `signing-enabled` flag ([`killswitch`]), (2) tx-shape
//! validation against the arb template ([`validate`]), and (3) synchronous, no-I/O pre-sign caps
//! ([`caps`]) — so worst-case outflow per window is bounded before lagging health metrics catch up.
//!
//! The canonical path is [`sidecar::SignerSidecar::sign_arb_tx`] (`flag → shape → caps → sign`,
//! atomic under one mutex). Thresholds for auto-halt live in the observ `HealthEvaluator`
//! (observ-14); the [`killswitch::apply_health_signal`] supervisor consumes that signal and owns the
//! flag. Networked pieces — the sweeper (signer-8, tokio+RPC) and Telegram/PagerDuty alert sinks
//! (reqwest) — attach behind the [`alert::AlertSink`] seam in their phase.

pub mod alert;
pub mod caps;
pub mod error;
pub mod keychain;
pub mod killswitch;
pub mod metrics;
pub mod presign;
pub mod sidecar;
pub mod sweeper;
pub mod validate;

pub use alert::{AlertMessage, AlertSink, LogSink, Severity};
pub use caps::{BalanceSnapshot, CapExceeded, CapReservation, PreSignCaps};
pub use error::SignerError;
pub use keychain::{BackendKind, MemorySigner, SolanaSigner};
pub use killswitch::{
    apply_health_signal, halt_reason_from_signal, HaltReason, KillSwitchHandle, RearmError,
    TripRecord,
};
pub use metrics::SignerMetrics;
pub use presign::{evaluate_pre_sign, PreSignDecision, PreSignGate};
pub use sidecar::SignerSidecar;
pub use sweeper::{decide_sweep, SweepDecision, SweepTrigger, SweeperConfig};
pub use validate::{ArbSignContext, ShapeReject, TxShapeValidator, ValidatedShape};
