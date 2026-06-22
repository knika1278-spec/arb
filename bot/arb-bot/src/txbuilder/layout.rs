//! Canonical `TryArbitrage` instruction layout (txbuilder-5 core).
//!
//! The instruction *data* is packed with the program's own `TryArbitrageData::pack` (we link
//! `arb-program`), so the wire bytes are byte-identical to what `unpack` expects — the ABI
//! cannot drift. The *account* list follows the canonical convention the processor reads:
//!
//! ```text
//! index 0           authority           (signer, writable)  — pays + owns the balance-read ATAs
//! [balance_read..]  base/intermediate ATAs (writable)       — profit-assert reads their deltas
//! [leg_a metas..]   leg-A swap CPI accounts (incl. program) — count = leg_a.account_count
//! [leg_b metas..]   leg-B swap CPI accounts (incl. program) — count = leg_b.account_count
//! ```
//!
//! Per the ABI, `leg_b.amount_in == 0` selects balance-delta chaining (leg B consumes the
//! measured intermediate output of leg A), so only `size_in` is pinned client-side.
//!
//! NOTE: the *content/order* of each leg's per-venue metas (pool, vaults, oracle, tick-arrays)
//! is resolved by detection's route-resolution (onchain-6/7 adapters + add-6 Whirlpool tick
//! resolver). This module owns the *framing* — ordering legs, counting accounts, packing data.

use crate::txbuilder::error::TxBuilderError;
use arb_program::instruction::{LegDescriptor, TryArbitrageData};
use arb_types::{DexKind, SwapDir};
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// One resolved leg: its venue, direction, per-leg slippage floor, and the full ordered list
/// of accounts its swap CPI consumes (the swap program account is part of this list).
#[derive(Clone, Debug)]
pub struct LegAccounts {
    pub dex: DexKind,
    pub dir: SwapDir,
    /// Minimum acceptable output for this leg (tolerance-adjusted by sizing).
    pub min_out: u64,
    /// Canonical per-venue account metas (incl. the swap program), exactly as the on-chain
    /// adapter consumes them.
    pub metas: Vec<AccountMeta>,
}

impl LegAccounts {
    fn descriptor(&self, amount_in: u64) -> Result<LegDescriptor, TxBuilderError> {
        let count = self.metas.len();
        if count == 0 {
            return Err(TxBuilderError::EmptyRoute);
        }
        if count > u8::MAX as usize {
            return Err(TxBuilderError::LegTooWide { got: count });
        }
        Ok(LegDescriptor {
            dex: self.dex,
            dir: self.dir,
            account_count: count as u8,
            amount_in,
            min_out: self.min_out,
        })
    }
}

/// Build the `TryArbitrage` instruction for a two-leg round trip.
///
/// * `size_in` — leg-A input amount (leg B uses the measured delta, `amount_in = 0`).
/// * `min_profit` — base-asset, costs-inclusive floor the round-trip must net (dec-3: one
///   definition shared by sizing, this builder, and the on-chain assert).
/// * `balance_read` — the profit-assert balance-read ATAs (base first, then intermediate).
pub fn build_arb_instruction(
    program_id: Pubkey,
    authority: Pubkey,
    size_in: u64,
    min_profit: u64,
    leg_a: &LegAccounts,
    leg_b: &LegAccounts,
    balance_read: &[AccountMeta],
) -> Result<Instruction, TxBuilderError> {
    let data = TryArbitrageData {
        min_profit,
        leg_a: leg_a.descriptor(size_in)?,
        leg_b: leg_b.descriptor(0)?, // balance-delta chaining
    }
    .pack()
    .to_vec();

    let mut accounts =
        Vec::with_capacity(1 + balance_read.len() + leg_a.metas.len() + leg_b.metas.len());
    accounts.push(AccountMeta::new(authority, true)); // authority: signer + writable
    accounts.extend_from_slice(balance_read);
    accounts.extend_from_slice(&leg_a.metas);
    accounts.extend_from_slice(&leg_b.metas);

    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_program::instruction::INSTRUCTION_LEN;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn leg(dex: DexKind, dir: SwapDir, n: u8) -> LegAccounts {
        LegAccounts {
            dex,
            dir,
            min_out: 100,
            metas: (0..n)
                .map(|i| AccountMeta::new(key(i + 50), false))
                .collect(),
        }
    }

    #[test]
    fn builds_instruction_with_canonical_framing() {
        let a = leg(DexKind::RaydiumCpmm, SwapDir::AtoB, 9);
        let b = leg(DexKind::OrcaWhirlpool, SwapDir::BtoA, 11);
        let base_ata = AccountMeta::new(key(10), false);
        let mid_ata = AccountMeta::new(key(11), false);
        let ix = build_arb_instruction(
            key(200),
            key(1),
            1_000_000,
            5_000,
            &a,
            &b,
            &[base_ata, mid_ata],
        )
        .unwrap();

        // authority + 2 balance-read + 9 + 11 = 23 accounts.
        assert_eq!(ix.accounts.len(), 1 + 2 + 9 + 11);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable);
        assert_eq!(ix.data.len(), INSTRUCTION_LEN);

        // Re-unpack with the program's own parser: framing is byte-faithful.
        let parsed = TryArbitrageData::unpack(&ix.data).unwrap();
        assert_eq!(parsed.min_profit, 5_000);
        assert_eq!(parsed.leg_a.account_count, 9);
        assert_eq!(parsed.leg_a.amount_in, 1_000_000);
        assert_eq!(parsed.leg_b.account_count, 11);
        assert_eq!(parsed.leg_b.amount_in, 0); // balance-delta chaining
        assert_eq!(parsed.leg_b.dex, DexKind::OrcaWhirlpool);
    }

    #[test]
    fn empty_leg_is_rejected() {
        let a = leg(DexKind::RaydiumCpmm, SwapDir::AtoB, 0);
        let b = leg(DexKind::OrcaWhirlpool, SwapDir::BtoA, 5);
        let err = build_arb_instruction(key(200), key(1), 1, 1, &a, &b, &[]).unwrap_err();
        assert_eq!(err, TxBuilderError::EmptyRoute);
    }
}
