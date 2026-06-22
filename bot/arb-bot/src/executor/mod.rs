//! Landing / executor (landing-3,6,8,10 + add-1): the final mile of the atomic-arb hot path.
//!
//! Host-green logic lives here — [`tip`] (TipOracle sizing), [`registry`] (the add-1 in-flight
//! writable-account dedupe BLOCKER), [`landing_loop`] (the fresh-blockhash rebuild state machine
//! with the [`landing_loop::BlockhashSource`] / [`landing_loop::LandingTransport`] seams), and
//! [`facade`] ([`facade::land`] wiring kill-switch → cost-gate → contention dedupe → tip → loop →
//! metrics). The networked clients (Jito Block Engine JSON-RPC, Helius Sender, SWQoS, RPC
//! `simulateTransaction`, tip_floor REST/WS) implement [`landing_loop::LandingTransport`] /
//! [`landing_loop::BlockhashSource`] in their phase (tokio + reqwest + solana-client w/ rustls);
//! the loop/facade are agnostic to them.

pub mod config;
pub mod facade;
pub mod landing_loop;
pub mod registry;
pub mod tip;
pub mod types;

pub use config::{ConfigError, ExecutorConfig};
pub use facade::{land, LandDeps, LandError, LandRequest, SignerHandle};
pub use landing_loop::{run_landing_loop, AttemptResult, BlockhashSource, LandingTransport};
pub use registry::{InflightGuard, WritableAccountRegistry};
pub use tip::{TipOracle, TipParams, MIN_TIP_LAMPORTS};
pub use types::{
    ArbTxSpec, Blockhash, BundleId, DropCause, InflightStatus, LandingOutcome, Region, Route,
    SenderMode, TipDecision, TipFloorSnapshot,
};
