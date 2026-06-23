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
use arb_config::limits::MAX_TX_ACCOUNT_LOCKS;
use arb_program::instruction::{
    LegDescriptor, TryArbitrageData, TryArbitrageNData, MAX_LEGS, MIN_LEGS,
};
use arb_types::{DexKind, SwapDir};
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::collections::HashSet;

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

/// txbuilder-15: build the N-leg (triangle+) `TryArbitrageN` instruction for a cycle
/// `base → t1 → … → t_{N-1} → base` of `2..=MAX_LEGS` legs. Mirrors [`build_arb_instruction`]'s
/// canonical framing, generalized: the account list is
/// `authority + cycle_atas(N) + concat(leg.metas)`, matching the N-leg processor (`onchain-20`).
///
/// * `size_in` — leg-0 input; every later leg uses the measured delta (`amount_in = 0`).
/// * `cycle_atas` — the N cycle ATAs in order (`ata[0]` = base, `ata[i]` = token i); exactly one
///   per leg. The terminal profit-assert reads `ata[0]`.
///
/// Enforces the account-lock budget up front (unique pubkeys `<= MAX_TX_ACCOUNT_LOCKS`) so a route
/// across 3 venues that cannot fit a single atomic tx is rejected before assembly; the full
/// serialized-byte / CU gate is the assembly-time [`crate::txbuilder::measure`] / `LimitReport`.
pub fn build_arb_n_instruction(
    program_id: Pubkey,
    authority: Pubkey,
    size_in: u64,
    min_profit: u64,
    legs: &[LegAccounts],
    cycle_atas: &[AccountMeta],
) -> Result<Instruction, TxBuilderError> {
    let n = legs.len();
    if !(MIN_LEGS..=MAX_LEGS).contains(&n) || cycle_atas.len() != n {
        return Err(TxBuilderError::BadLegCount {
            got: n,
            min: MIN_LEGS,
            max: MAX_LEGS,
        });
    }

    // Account-lock budget: reject a too-wide route before it is ever assembled/signed.
    let locks = route_account_locks(authority, cycle_atas, legs);
    if locks > MAX_TX_ACCOUNT_LOCKS {
        return Err(TxBuilderError::TooManyAccountLocks {
            got: locks,
            max: MAX_TX_ACCOUNT_LOCKS,
        });
    }

    // Descriptors: leg 0 takes the explicit sizing input; the rest chain via `amount_in = 0`.
    let mut descriptors = Vec::with_capacity(n);
    for (i, leg) in legs.iter().enumerate() {
        let amount_in = if i == 0 { size_in } else { 0 };
        descriptors.push(leg.descriptor(amount_in)?);
    }
    let nd = TryArbitrageNData::from_legs(min_profit, &descriptors).ok_or(
        TxBuilderError::BadLegCount {
            got: n,
            min: MIN_LEGS,
            max: MAX_LEGS,
        },
    )?;
    let (buf, used) = nd.pack();
    let data = buf[..used].to_vec();

    let total_metas: usize = legs.iter().map(|l| l.metas.len()).sum();
    let mut accounts = Vec::with_capacity(1 + cycle_atas.len() + total_metas);
    accounts.push(AccountMeta::new(authority, true)); // authority: signer + writable
    accounts.extend_from_slice(cycle_atas);
    for leg in legs {
        accounts.extend_from_slice(&leg.metas);
    }

    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}

/// Count the UNIQUE account locks a route touches (authority + cycle ATAs + every leg meta). v0
/// txs lock each unique key once regardless of repetition, so dedupe before checking the ceiling.
fn route_account_locks(
    authority: Pubkey,
    cycle_atas: &[AccountMeta],
    legs: &[LegAccounts],
) -> usize {
    let mut set: HashSet<Pubkey> = HashSet::new();
    set.insert(authority);
    for m in cycle_atas {
        set.insert(m.pubkey);
    }
    for leg in legs {
        for m in &leg.metas {
            set.insert(m.pubkey);
        }
    }
    set.len()
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

    fn ata(b: u8) -> AccountMeta {
        AccountMeta::new(key(b), false)
    }

    #[test]
    fn builds_three_leg_instruction_with_canonical_framing() {
        // A Meteora DAMM v2 → DLMM → Raydium CLMM triangle (the ANB-style heterogeneous cycle).
        let l0 = leg(DexKind::MeteoraDammV2, SwapDir::AtoB, 8);
        let l1 = leg(DexKind::MeteoraDlmm, SwapDir::AtoB, 10);
        let l2 = leg(DexKind::RaydiumClmm, SwapDir::BtoA, 12);
        let atas = [ata(10), ata(11), ata(12)]; // base, t1, t2
        let ix = build_arb_n_instruction(key(200), key(1), 1_000_000, 7_000, &[l0, l1, l2], &atas)
            .unwrap();

        // authority + 3 cycle ATAs + 8 + 10 + 12 = 34 accounts.
        assert_eq!(ix.accounts.len(), 1 + 3 + 8 + 10 + 12);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable);

        // Re-unpack with the program's own N-leg parser: framing is byte-faithful.
        let parsed = TryArbitrageNData::unpack(&ix.data).unwrap();
        assert_eq!(parsed.leg_count, 3);
        assert_eq!(parsed.min_profit, 7_000);
        assert_eq!(parsed.legs()[0].amount_in, 1_000_000);
        assert_eq!(parsed.legs()[1].amount_in, 0); // balance-delta chaining
        assert_eq!(parsed.legs()[2].amount_in, 0);
        assert_eq!(parsed.legs()[2].dex, DexKind::RaydiumClmm);
    }

    #[test]
    fn rejects_bad_leg_count_and_ata_mismatch() {
        let l0 = leg(DexKind::RaydiumCpmm, SwapDir::AtoB, 5);
        let l1 = leg(DexKind::OrcaWhirlpool, SwapDir::BtoA, 5);
        // 1 leg < MIN_LEGS.
        assert!(matches!(
            build_arb_n_instruction(key(200), key(1), 1, 1, &[l0.clone()], &[ata(10)]),
            Err(TxBuilderError::BadLegCount { .. })
        ));
        // 2 legs but 3 cycle ATAs (must be one per leg).
        assert!(matches!(
            build_arb_n_instruction(
                key(200),
                key(1),
                1,
                1,
                &[l0, l1],
                &[ata(10), ata(11), ata(12)]
            ),
            Err(TxBuilderError::BadLegCount { .. })
        ));
    }

    #[test]
    fn rejects_route_exceeding_account_lock_budget() {
        // Two legs whose combined UNIQUE accounts exceed the 128 lock ceiling.
        let wide = |start: u8, n: u16| -> LegAccounts {
            LegAccounts {
                dex: DexKind::RaydiumClmm,
                dir: SwapDir::AtoB,
                min_out: 1,
                metas: (0..n)
                    .map(|i| {
                        let mut a = [0u8; 32];
                        a[0] = start;
                        a[1] = (i & 0xff) as u8;
                        a[2] = (i >> 8) as u8;
                        AccountMeta::new(Pubkey::new_from_array(a), false)
                    })
                    .collect(),
            }
        };
        let err = build_arb_n_instruction(
            key(200),
            key(1),
            1,
            1,
            &[wide(1, 70), wide(2, 70)], // 140 unique + authority + 2 ATAs = 143 > 128
            &[ata(10), ata(11)],
        )
        .unwrap_err();
        assert!(matches!(err, TxBuilderError::TooManyAccountLocks { got, max } if got > max));
    }
}
