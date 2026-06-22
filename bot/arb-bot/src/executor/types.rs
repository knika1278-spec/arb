//! Shared executor/landing data types (landing module `types.rs`).

use solana_pubkey::Pubkey;

/// The 8 Jito Block Engine regions (plan §6 endpoint list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Ny,
    Amsterdam,
    Dublin,
    Frankfurt,
    London,
    Slc,
    Singapore,
    Tokyo,
}

impl Region {
    pub const ALL: [Region; 8] = [
        Region::Ny,
        Region::Amsterdam,
        Region::Dublin,
        Region::Frankfurt,
        Region::London,
        Region::Slc,
        Region::Singapore,
        Region::Tokyo,
    ];

    /// Regional Block Engine host (`https://<host>/api/v1/bundles`).
    pub const fn host(self) -> &'static str {
        match self {
            Region::Ny => "ny.mainnet.block-engine.jito.wtf",
            Region::Amsterdam => "amsterdam.mainnet.block-engine.jito.wtf",
            Region::Dublin => "dublin.mainnet.block-engine.jito.wtf",
            Region::Frankfurt => "frankfurt.mainnet.block-engine.jito.wtf",
            Region::London => "london.mainnet.block-engine.jito.wtf",
            Region::Slc => "slc.mainnet.block-engine.jito.wtf",
            Region::Singapore => "singapore.mainnet.block-engine.jito.wtf",
            Region::Tokyo => "tokyo.mainnet.block-engine.jito.wtf",
        }
    }
}

/// Helius Sender mode (plan §6): `swqos_only` (cheap, 0.000005 SOL min tip) vs `dual` (SWQoS+Jito,
/// 0.0002 SOL min tip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SenderMode {
    SwqosOnly,
    Dual,
}

impl SenderMode {
    /// Minimum tip in lamports required by this mode.
    pub const fn min_tip_lamports(self) -> u64 {
        match self {
            SenderMode::SwqosOnly => 5_000, // 0.000005 SOL
            SenderMode::Dual => 200_000,    // 0.0002 SOL
        }
    }
}

/// Landing route taken. The routing-exclusivity invariant: a tx carrying a `jitodontfront` marker
/// must NEVER leave via a non-Jito path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    JitoBundle { region: Region },
    HeliusSender { mode: SenderMode },
    Swqos,
}

impl Route {
    /// Whether this route provides Jito Block-Engine front-run protection.
    pub const fn is_jito_protected(self) -> bool {
        matches!(self, Route::JitoBundle { .. })
    }
}

/// Best-effort attribution of why an attempt did not land (co-dominant per plan §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropCause {
    TipAuctionLost,
    Congestion,
    TooLateInSlot,
    SimFailed,
    StaleBlockhash,
    UncledOrSkipped,
    SenderRejected,
    RateLimited,
    /// add-1: a second opportunity on the same writable pool was gated.
    WritableContention,
    Unknown,
}

/// SHA-256 of a bundle's signatures — a receipt, NOT a landing guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BundleId(pub [u8; 32]);

/// Inflight status from `getInflightBundleStatuses` (~5 min window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InflightStatus {
    Invalid,
    Pending,
    Failed,
    Landed,
    NotFound,
}

/// A 32-byte recent blockhash. Newtype keeps the landing loop free of a `solana-message` dep until
/// the real `BlockhashSource` lands (the executor compiles the v0 message at the seam).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Blockhash(pub [u8; 32]);

/// Live tip-floor percentiles (lamports) from `bundles.jito.wtf/tip_floor` + WS `tip_stream`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipFloorSnapshot {
    pub p25: u64,
    pub p50: u64,
    pub p75: u64,
    pub p95: u64,
    pub p99: u64,
    pub ema: u64,
    /// Logical timestamp (ms) the snapshot was taken.
    pub at_millis: u64,
}

/// Result of `TipOracle::size_tip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipDecision {
    pub lamports: u64,
    /// Percentile in `[0,1]` the baseline used (informational).
    pub percentile_used: u32,
    /// Whether the profit-fraction cap clamped the tip.
    pub capped_by_profit: bool,
    pub account: Pubkey,
}

/// The REBUILDABLE description of one arb attempt (re-sim, re-size tip, re-sign across rebuilds).
#[derive(Clone, Debug)]
pub struct ArbTxSpec {
    pub payer: Pubkey,
    pub cu_limit: u32,
    pub cu_price_micro: u64,
    pub sim_profit_lamports: u64,
    /// Writable pool pubkeys this attempt locks (add-1 contention key).
    pub route_pools: Vec<Pubkey>,
    /// ALT tables to attach when compiling the v0 message (seam).
    pub alt_tables: Vec<Pubkey>,
}

/// Terminal outcome of a landing attempt sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandingOutcome {
    Landed {
        slot: u64,
        attempts: u8,
        tip_paid_lamports: u64,
        route: Route,
        latency_ms: u64,
    },
    /// Landed on-chain but the terminal assert reverted (base+priority burned, tip unpaid).
    Reverted {
        slot: u64,
        attempts: u8,
        burned_lamports: u64,
    },
    /// Never landed within the deadline/attempt budget.
    GaveUp { attempts: u8, last_cause: DropCause },
}
