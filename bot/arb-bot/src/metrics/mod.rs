//! Observability & economics (observ-1..9): first-class health/PnL telemetry + the probabilistic
//! pre-sign cost-gate. Lock-free hot-path counters ([`registry`]), latency P50/P95 ([`latency`]),
//! realized slippage per route ([`slippage`]), PnL + burn-rate ([`pnl`]), the deterministic
//! cost-gate + `p_land` EWMA ([`econ`]), and the threshold-driven kill-switch signal ([`health`]).
//!
//! The off-process surfaces — Prometheus exporter (observ-7), Telegram/PagerDuty alert router
//! (observ-8), and the analytics backtest/golden-replay gate (observ-10..12) — are network/CLI
//! bound (`hyper`/`reqwest`/`clap`) and land in their own phase; this in-process pipeline is the
//! producer they read. The signer's synchronous pre-sign gate consumes [`econ::CostModel::gate`]
//! and the supervisor consumes [`health::KillSwitchSignal`] (the observ-14 integration seam).

pub mod alerts;
pub mod econ;
pub mod exporter;
pub mod health;
pub mod latency;
pub mod pnl;
pub mod registry;
pub mod slippage;
pub mod types;

pub use alerts::AlertRouter;
pub use econ::{
    CostGateDecision, CostInputs, CostModel, EconParams, PLandEstimator, RejectReason, TipBucket,
};
pub use exporter::{healthz_json, prometheus_exposition, serve_blocking};
pub use health::{HealthEvaluator, HealthSnapshot, KillSwitchSignal, Thresholds, TripReason};
pub use latency::{ConfirmationRank, LatencyBook, LatencyStage, SpanGuard};
pub use pnl::{PnlError, PnlLedger, TxOutcome};
pub use registry::{CounterSnapshot, MetricsRegistry};
pub use slippage::{slippage_bps, SlipStats, SlippageBook};
pub use types::{RevertCause, RouteKey, TxKind};
