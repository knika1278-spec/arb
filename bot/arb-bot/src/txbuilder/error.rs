//! Error taxonomy for the tx-builder. Every variant maps to a *pre-sign* rejection — the
//! builder's whole job is to fail loudly on the host before a malformed/oversized/unsafe tx
//! ever reaches the signer. The on-chain assert is the final net (invariant §2); these are
//! the cheap guards that stop us paying fees to discover the same thing on-chain.

use solana_pubkey::Pubkey;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TxBuilderError {
    /// Account-lock count exceeds [`arb_config::limits::MAX_TX_ACCOUNT_LOCKS`] (the ceiling
    /// that binds BEFORE the 256 loaded-accounts cap — invariant §5).
    #[error("account-lock count {got} exceeds ceiling {max}")]
    TooManyAccountLocks { got: usize, max: usize },

    /// Unique loaded accounts exceed [`arb_config::limits::MAX_LOADED_ACCOUNTS`].
    #[error("loaded-account count {got} exceeds {max}")]
    TooManyLoadedAccounts { got: usize, max: usize },

    /// Serialized transaction size exceeds the 1232-byte cap (NOT raised by an ALT).
    #[error("serialized tx size {got} B exceeds {max} B cap")]
    TxTooLarge { got: usize, max: usize },

    /// Requested compute-unit limit exceeds [`arb_config::limits::MAX_COMPUTE_UNIT_LIMIT`].
    #[error("compute-unit limit {got} exceeds {max}")]
    ComputeBudgetExceeded { got: u32, max: u32 },

    /// A routed mint carries a HARD-REJECT Token-2022 extension (mirror of the on-chain
    /// `token2022::vet_mint` — invariant §8).
    #[error("mint {mint} carries a forbidden Token-2022 extension")]
    ForbiddenTokenExtension { mint: Pubkey },

    /// A swap-CPI target is not in the Wave-1 DEX allowlist (trust-boundary mirror, §6).
    #[error("swap program {program} is not in the Wave-1 DEX allowlist")]
    UnauthorizedSwapProgram { program: Pubkey },

    /// A profit-checked / balance-read token account is not the bot-owned ATA (§6b).
    #[error("destination token account {account} is not the bot-owned ATA (expected {expected})")]
    UnownedDestination { account: Pubkey, expected: Pubkey },

    /// A routed token account is frozen — the leg would fail on-chain.
    #[error("token account {account} is frozen")]
    FrozenAccount { account: Pubkey },

    /// An ALT was extended in the current slot — using it now risks v0 key-resolution
    /// failure (invariant §4: never extend-then-use in the same slot).
    #[error("ALT {table} extended at slot {last_extended_slot} not warm at slot {current_slot}")]
    AltNotWarm {
        table: Pubkey,
        last_extended_slot: u64,
        current_slot: u64,
    },

    /// A leg had no resolved accounts, or the route had fewer than two legs.
    #[error("empty / under-specified route")]
    EmptyRoute,

    /// A single leg referenced more than 255 accounts (cannot fit the u8 `account_count`).
    #[error("leg account count {got} exceeds 255 (u8 account_count)")]
    LegTooWide { got: usize },

    /// Predicted realized profit is below the required `min_profit` floor (costs-inclusive).
    #[error("predicted profit {predicted} below required min_profit {min_profit}")]
    BelowMinProfit { predicted: i128, min_profit: u64 },

    /// Preflight simulation returned a program error code (decode against `arb_types::ArbError`).
    #[error("preflight simulation reverted with custom error {code:?}")]
    SimulationReverted { code: Option<u32> },

    /// The Jito tip exceeds the profit-fraction cap (txbuilder-13 — defense-in-depth mirror of the
    /// executor's TipOracle cap so an over-tip cannot be assembled even if the oracle misbehaves).
    #[error("tip {tip} exceeds the profit-fraction cap {cap} (profit {profit})")]
    TipExceedsCap { tip: u64, cap: u64, profit: u64 },

    /// v0 message compilation failed (e.g. a signer key landed in an ALT, or too many
    /// account keys). Stores `solana_message::CompileError` Display (it is not Clone/Eq).
    #[error("v0 message compile failed: {0}")]
    MessageCompile(String),
}
