//! Yellowstone gRPC ingest seam. The real client (`yellowstone-grpc-client`, subscribe by
//! `owner`/`memcmp`, commitment `processed`) is wired in the run-loop task and is kept behind
//! this trait so the pipeline is testable with a Vec-backed mock and so the heavy gRPC/tonic
//! dependency does not enter the build until that module lands.

use solana_pubkey::Pubkey;

/// A raw streamed account write, before venue decoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawAccountUpdate {
    pub pubkey: Pubkey,
    /// Owning program (used to pick the venue decoder).
    pub owner: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
    pub write_version: u64,
}

/// Pollable source of account updates. Implemented by the real Yellowstone client and by
/// test mocks alike.
pub trait AccountUpdateSource {
    /// Next available update, or `None` if none is ready (non-blocking).
    fn try_next(&mut self) -> Option<RawAccountUpdate>;
}

/// Test/double: drains a queue of pre-baked updates.
#[derive(Default)]
pub struct MockSource {
    pub queue: std::collections::VecDeque<RawAccountUpdate>,
}

impl AccountUpdateSource for MockSource {
    fn try_next(&mut self) -> Option<RawAccountUpdate> {
        self.queue.pop_front()
    }
}
