//! observ-10 — golden-replay corpus format + loader.
//!
//! A [`GoldenSample`] is a frozen historical opportunity (winner OR loser) captured from a Geyser
//! snapshot / Old Faithful: the two CPMM pool states, the route direction, the exact `amount_in`,
//! the recorded realized output + land/revert outcome, and the economic terms. The replay
//! ([`super::replay`]) re-prices each sample through the SAME `arb-math` mirror the bot runs and
//! asserts predicted == recorded; the corpus MUST include losers so loser-burn is covered.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One CPMM pool's reserves + fee (oriented `reserve_a ↔ mint_a`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolSnapshot {
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub fee_num: u64,
    pub fee_den: u64,
}

/// A frozen historical opportunity for the regression gate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GoldenSample {
    pub id: String,
    pub slot: u64,
    pub pool_a: PoolSnapshot,
    pub pool_b: PoolSnapshot,
    /// Leg-A direction (`SwapDir` tag: 0 = AtoB, 1 = BtoA).
    pub dir_a: u8,
    /// Leg-B direction (`SwapDir` tag).
    pub dir_b: u8,
    pub amount_in: u64,
    /// The realized base-out recorded on-chain (balance delta).
    pub recorded_realized_out: u64,
    /// Whether the recorded attempt landed (false = reverting loser).
    pub recorded_landed: bool,
    // ---- economic terms (lamports), for the backtest E[net] ----
    pub spread_lamports: u64,
    pub swap_fees_lamports: u64,
    pub tip_lamports: u64,
    pub prio_lamports: u64,
    pub base_lamports: u64,
    pub p_land: f64,
}

/// Corpus load/parse error.
#[derive(Debug)]
pub enum CorpusError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorpusError::Io(e) => write!(f, "corpus io error: {e}"),
            CorpusError::Json(e) => write!(f, "corpus json error: {e}"),
        }
    }
}

impl std::error::Error for CorpusError {}

/// Parse a JSON array of samples.
pub fn parse_corpus(json: &str) -> Result<Vec<GoldenSample>, CorpusError> {
    serde_json::from_str(json).map_err(CorpusError::Json)
}

/// Load a corpus file (JSON array of [`GoldenSample`]).
pub fn load_corpus(path: &Path) -> Result<Vec<GoldenSample>, CorpusError> {
    let raw = std::fs::read_to_string(path).map_err(CorpusError::Io)?;
    parse_corpus(&raw)
}

#[cfg(test)]
pub(crate) fn sample_fixture() -> Vec<GoldenSample> {
    // One winner (lands) and one loser (reverts) — loser-burn coverage.
    let winner = GoldenSample {
        id: "winner-1".into(),
        slot: 100,
        pool_a: PoolSnapshot {
            reserve_a: 1_000_000,
            reserve_b: 2_000_000,
            fee_num: 25,
            fee_den: 10_000,
        },
        pool_b: PoolSnapshot {
            reserve_a: 2_000_000,
            reserve_b: 1_100_000,
            fee_num: 25,
            fee_den: 10_000,
        },
        dir_a: 0,
        dir_b: 0,
        amount_in: 50_000,
        recorded_realized_out: 0, // filled by the test from the mirror
        recorded_landed: true,
        spread_lamports: 20_000,
        swap_fees_lamports: 2_000,
        tip_lamports: 3_000,
        prio_lamports: 2_000,
        base_lamports: 5_000,
        p_land: 0.8,
    };
    let loser = GoldenSample {
        id: "loser-1".into(),
        recorded_landed: false,
        spread_lamports: 1_000,
        ..winner.clone()
    };
    vec![winner, loser]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_winner_and_loser_corpus() {
        let json = serde_json::to_string(&sample_fixture()).unwrap();
        let parsed = parse_corpus(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].recorded_landed); // winner
        assert!(!parsed[1].recorded_landed); // loser (burn coverage)
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_corpus("{not json").is_err());
    }
}
