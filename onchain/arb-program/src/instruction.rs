//! `TryArbitrage` instruction-data layout (the program's ABI). Little-endian, fixed-size so
//! the hot path parses with bounds-checked slices and no Borsh. The client (tx-builder) and
//! the signer's tx-shape validator pack/derive against this exact layout.
//!
//! ```text
//! offset  size  field
//! 0       1     tag (0 = TryArbitrage)
//! 1       8     min_profit: u64   (base-asset units the round-trip must net, on top of costs)
//! 9       20    leg_a: LegDescriptor
//! 29      20    leg_b: LegDescriptor
//! ```
//! `LegDescriptor` = `dex:u8, dir:u8, account_count:u8, _pad:u8, amount_in:u64, min_out:u64`.
//! For `leg_b`, `amount_in == 0` means "use the measured intermediate balance delta from
//! leg A" (balance-delta chaining, invariant §7).

use crate::error::to_program_error;
use arb_types::{ArbError, DexKind, SwapDir};
use solana_program::program_error::ProgramError;

pub const TAG_TRY_ARBITRAGE: u8 = 0;
pub const LEG_LEN: usize = 20;
pub const INSTRUCTION_LEN: usize = 1 + 8 + LEG_LEN * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegDescriptor {
    pub dex: DexKind,
    pub dir: SwapDir,
    /// Number of `remaining_accounts` this leg's swap CPI consumes.
    pub account_count: u8,
    pub amount_in: u64,
    pub min_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TryArbitrageData {
    pub min_profit: u64,
    pub leg_a: LegDescriptor,
    pub leg_b: LegDescriptor,
}

fn read_u64(data: &[u8], at: usize) -> Result<u64, ProgramError> {
    let bytes = data
        .get(at..at.saturating_add(8))
        .ok_or_else(|| to_program_error(ArbError::MalformedInstructionData))?;
    let arr: [u8; 8] = bytes
        .try_into()
        .map_err(|_| to_program_error(ArbError::MalformedInstructionData))?;
    Ok(u64::from_le_bytes(arr))
}

fn unpack_leg(data: &[u8], at: usize) -> Result<LegDescriptor, ProgramError> {
    let bad = || to_program_error(ArbError::MalformedInstructionData);
    let dex_tag = *data.get(at).ok_or_else(bad)?;
    let dir_tag = *data.get(at.saturating_add(1)).ok_or_else(bad)?;
    let account_count = *data.get(at.saturating_add(2)).ok_or_else(bad)?;
    let dex = DexKind::from_tag(dex_tag).ok_or_else(bad)?;
    let dir = SwapDir::from_tag(dir_tag).ok_or_else(bad)?;
    let amount_in = read_u64(data, at.saturating_add(4))?;
    let min_out = read_u64(data, at.saturating_add(12))?;
    Ok(LegDescriptor {
        dex,
        dir,
        account_count,
        amount_in,
        min_out,
    })
}

impl TryArbitrageData {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        let bad = || to_program_error(ArbError::MalformedInstructionData);
        if data.len() < INSTRUCTION_LEN {
            return Err(bad());
        }
        if data[0] != TAG_TRY_ARBITRAGE {
            return Err(bad());
        }
        let min_profit = read_u64(data, 1)?;
        let leg_a = unpack_leg(data, 9)?;
        let leg_b = unpack_leg(data, 9 + LEG_LEN)?;
        Ok(Self {
            min_profit,
            leg_a,
            leg_b,
        })
    }

    /// Symmetric packer (used by the client/tests).
    pub fn pack(&self) -> [u8; INSTRUCTION_LEN] {
        let mut out = [0u8; INSTRUCTION_LEN];
        out[0] = TAG_TRY_ARBITRAGE;
        out[1..9].copy_from_slice(&self.min_profit.to_le_bytes());
        pack_leg(&mut out, 9, &self.leg_a);
        pack_leg(&mut out, 9 + LEG_LEN, &self.leg_b);
        out
    }
}

fn pack_leg(out: &mut [u8; INSTRUCTION_LEN], at: usize, leg: &LegDescriptor) {
    out[at] = leg.dex.tag();
    out[at + 1] = leg.dir.tag();
    out[at + 2] = leg.account_count;
    out[at + 4..at + 12].copy_from_slice(&leg.amount_in.to_le_bytes());
    out[at + 12..at + 20].copy_from_slice(&leg.min_out.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let d = TryArbitrageData {
            min_profit: 123_456,
            leg_a: LegDescriptor {
                dex: DexKind::RaydiumCpmm,
                dir: SwapDir::AtoB,
                account_count: 9,
                amount_in: 1_000_000,
                min_out: 999_000,
            },
            leg_b: LegDescriptor {
                dex: DexKind::OrcaWhirlpool,
                dir: SwapDir::BtoA,
                account_count: 11,
                amount_in: 0, // use measured delta
                min_out: 1_001_000,
            },
        };
        let bytes = d.pack();
        assert_eq!(bytes.len(), INSTRUCTION_LEN);
        assert_eq!(TryArbitrageData::unpack(&bytes).unwrap(), d);
    }

    #[test]
    fn rejects_short_or_bad_tag() {
        assert!(TryArbitrageData::unpack(&[0u8; 10]).is_err());
        let mut bytes = TryArbitrageData {
            min_profit: 1,
            leg_a: LegDescriptor {
                dex: DexKind::RaydiumCpmm,
                dir: SwapDir::AtoB,
                account_count: 1,
                amount_in: 1,
                min_out: 1,
            },
            leg_b: LegDescriptor {
                dex: DexKind::PumpSwapAmm,
                dir: SwapDir::AtoB,
                account_count: 1,
                amount_in: 1,
                min_out: 1,
            },
        }
        .pack()
        .to_vec();
        bytes[0] = 7; // bad tag
        assert!(TryArbitrageData::unpack(&bytes).is_err());
    }
}
