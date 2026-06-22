//! `arb_bot` library: the off-chain hot-path modules (detection, sizing, and — as they land
//! — txbuilder, executor, signer, metrics). The `arb-bot` binary (`main.rs`) is a thin
//! launcher over this library. Splitting lib/bin lets every module be unit-tested directly.

pub mod detection;
pub mod executor;
pub mod metrics;
pub mod signer;
pub mod sizing;
pub mod txbuilder;
