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
/// onchain-20: N-leg (triangle+) variant. Distinct tag so the proven 2-leg path (tag 0) is
/// never reinterpreted; this path is gated by `M1-GATE-EXT` / `testing-11`.
pub const TAG_TRY_ARBITRAGE_N: u8 = 1;
pub const LEG_LEN: usize = 20;
pub const INSTRUCTION_LEN: usize = 1 + 8 + LEG_LEN * 2;

/// N-leg header: `tag(1) + min_profit(8) + leg_count(1)`.
pub const N_HEADER_LEN: usize = 1 + 8 + 1;
/// Smallest cycle the N-leg path accepts (a 2-leg round-trip; the 3-leg triangle is the
/// motivating case).
pub const MIN_LEGS: usize = 2;
/// Account-budget bound on the cycle length (locks<128 / tx≤1232B across N venues — enforced in
/// detail by `txbuilder-15`). The triangle is 3; 4 leaves head-room without risking the budget.
pub const MAX_LEGS: usize = 4;
/// Largest possible packed N-leg instruction.
pub const MAX_N_INSTRUCTION_LEN: usize = N_HEADER_LEN + LEG_LEN * MAX_LEGS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegDescriptor {
    pub dex: DexKind,
    pub dir: SwapDir,
    /// Number of `remaining_accounts` this leg's swap CPI consumes.
    pub account_count: u8,
    pub amount_in: u64,
    pub min_out: u64,
}

impl LegDescriptor {
    /// Inert filler for the unused tail of a fixed-capacity leg array (never read: only the
    /// first `leg_count` entries are meaningful).
    const PLACEHOLDER: Self = Self {
        dex: DexKind::RaydiumCpmm,
        dir: SwapDir::AtoB,
        account_count: 0,
        amount_in: 0,
        min_out: 0,
    };
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

fn pack_leg(out: &mut [u8], at: usize, leg: &LegDescriptor) {
    out[at] = leg.dex.tag();
    out[at + 1] = leg.dir.tag();
    out[at + 2] = leg.account_count;
    out[at + 4..at + 12].copy_from_slice(&leg.amount_in.to_le_bytes());
    out[at + 12..at + 20].copy_from_slice(&leg.min_out.to_le_bytes());
}

/// onchain-20: the N-leg (triangle+) instruction. A cycle `base → t1 → … → t_{N-1} → base` of
/// `leg_count` swaps; leg `i` swaps `ata[i] → ata[(i+1) mod N]`. Layout:
/// ```text
/// 0       1        tag (1 = TryArbitrageN)
/// 1       8        min_profit: u64
/// 9       1        leg_count: u8   (MIN_LEGS..=MAX_LEGS)
/// 10      20*N     legs: [LegDescriptor; leg_count]
/// ```
/// As in the 2-leg path, a leg's `amount_in == 0` (for any non-first leg) means "use the
/// measured balance delta the previous leg produced into this leg's input ATA" (invariant §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TryArbitrageNData {
    pub min_profit: u64,
    pub leg_count: u8,
    /// Only `legs[..leg_count]` are meaningful; the tail is [`LegDescriptor::PLACEHOLDER`].
    pub legs: [LegDescriptor; MAX_LEGS],
}

impl TryArbitrageNData {
    /// Build from a slice of `MIN_LEGS..=MAX_LEGS` legs (the client/tx-builder entry). `None` if
    /// the leg count is out of range; fills the unused tail with [`LegDescriptor::PLACEHOLDER`].
    pub fn from_legs(min_profit: u64, legs: &[LegDescriptor]) -> Option<Self> {
        let n = legs.len();
        if !(MIN_LEGS..=MAX_LEGS).contains(&n) {
            return None;
        }
        let mut arr = [LegDescriptor::PLACEHOLDER; MAX_LEGS];
        arr[..n].copy_from_slice(legs);
        Some(Self {
            min_profit,
            leg_count: n as u8,
            legs: arr,
        })
    }

    /// The meaningful legs (`&legs[..leg_count]`).
    pub fn legs(&self) -> &[LegDescriptor] {
        &self.legs[..self.leg_count as usize]
    }

    /// Bytes a `pack()` of this descriptor occupies.
    pub fn packed_len(&self) -> usize {
        N_HEADER_LEN + LEG_LEN * self.leg_count as usize
    }

    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        let bad = || to_program_error(ArbError::MalformedInstructionData);
        if data.len() < N_HEADER_LEN {
            return Err(bad());
        }
        if data[0] != TAG_TRY_ARBITRAGE_N {
            return Err(bad());
        }
        let min_profit = read_u64(data, 1)?;
        let leg_count = data[9] as usize;
        if !(MIN_LEGS..=MAX_LEGS).contains(&leg_count) {
            return Err(bad());
        }
        let needed = N_HEADER_LEN
            .checked_add(LEG_LEN.checked_mul(leg_count).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        if data.len() < needed {
            return Err(bad());
        }
        let mut legs = [LegDescriptor::PLACEHOLDER; MAX_LEGS];
        for (i, slot) in legs.iter_mut().enumerate().take(leg_count) {
            *slot = unpack_leg(data, N_HEADER_LEN + i * LEG_LEN)?;
        }
        Ok(Self {
            min_profit,
            leg_count: leg_count as u8,
            legs,
        })
    }

    /// Symmetric packer (client/tests): writes into a max-size buffer, returns `(buf, used_len)`.
    pub fn pack(&self) -> ([u8; MAX_N_INSTRUCTION_LEN], usize) {
        let mut out = [0u8; MAX_N_INSTRUCTION_LEN];
        out[0] = TAG_TRY_ARBITRAGE_N;
        out[1..9].copy_from_slice(&self.min_profit.to_le_bytes());
        out[9] = self.leg_count;
        for i in 0..self.leg_count as usize {
            pack_leg(&mut out, N_HEADER_LEN + i * LEG_LEN, &self.legs[i]);
        }
        (out, self.packed_len())
    }
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

    fn leg(dex: DexKind, dir: SwapDir, ac: u8, amt: u64, min: u64) -> LegDescriptor {
        LegDescriptor {
            dex,
            dir,
            account_count: ac,
            amount_in: amt,
            min_out: min,
        }
    }

    #[test]
    fn n_leg_pack_unpack_roundtrip_triangle() {
        let mut legs = [LegDescriptor::PLACEHOLDER; MAX_LEGS];
        legs[0] = leg(DexKind::MeteoraDammV2, SwapDir::AtoB, 12, 1_000_000, 1);
        legs[1] = leg(DexKind::MeteoraDlmm, SwapDir::AtoB, 14, 0, 1); // 0 => measured delta
        legs[2] = leg(DexKind::RaydiumClmm, SwapDir::BtoA, 15, 0, 1_001_000);
        let d = TryArbitrageNData {
            min_profit: 42_000,
            leg_count: 3,
            legs,
        };
        let (buf, len) = d.pack();
        assert_eq!(len, N_HEADER_LEN + LEG_LEN * 3);
        let got = TryArbitrageNData::unpack(&buf[..len]).unwrap();
        assert_eq!(got, d);
        assert_eq!(got.legs().len(), 3);
        assert_eq!(got.legs()[2].dex, DexKind::RaydiumClmm);
    }

    #[test]
    fn n_leg_rejects_bad_count_and_short_buffer() {
        // leg_count below MIN / above MAX.
        let mut hdr = [0u8; MAX_N_INSTRUCTION_LEN];
        hdr[0] = TAG_TRY_ARBITRAGE_N;
        hdr[9] = 1; // < MIN_LEGS
        assert!(TryArbitrageNData::unpack(&hdr).is_err());
        hdr[9] = (MAX_LEGS + 1) as u8;
        assert!(TryArbitrageNData::unpack(&hdr).is_err());
        // Valid count but the buffer is too short to hold the legs.
        hdr[9] = 3;
        assert!(TryArbitrageNData::unpack(&hdr[..N_HEADER_LEN + LEG_LEN]).is_err());
        // Wrong tag is rejected.
        let mut legs = [LegDescriptor::PLACEHOLDER; MAX_LEGS];
        legs[0] = leg(DexKind::RaydiumCpmm, SwapDir::AtoB, 1, 1, 1);
        legs[1] = leg(DexKind::PumpSwapAmm, SwapDir::BtoA, 1, 0, 1);
        let (mut buf, len) = (TryArbitrageNData {
            min_profit: 1,
            leg_count: 2,
            legs,
        })
        .pack();
        buf[0] = TAG_TRY_ARBITRAGE; // 2-leg tag must not parse as N-leg
        assert!(TryArbitrageNData::unpack(&buf[..len]).is_err());
    }
}
