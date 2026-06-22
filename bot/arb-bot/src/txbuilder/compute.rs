//! ComputeBudget instruction builder + measured-CU sizing (txbuilder-2).
//!
//! The ComputeBudget program has no IDL we depend on; its instruction encoding is a stable,
//! tiny Borsh enum (`agave` `compute-budget-interface`): a 1-byte variant tag followed by the
//! little-endian payload. We hand-encode it to avoid pulling another solana crate.
//!
//! ```text
//! 0x01 RequestHeapFrame(u32)               (unused for M1)
//! 0x02 SetComputeUnitLimit(u32)            -> cap CU so we don't get the 200k/ix default
//! 0x03 SetComputeUnitPrice(u64 µlamports)  -> priority fee per CU
//! 0x04 SetLoadedAccountsDataSizeLimit(u32) (unused for M1)
//! ```
//! Over-requesting CU = overpaying priority fee, so the limit is sized from a *measured*
//! simulation figure plus the tight ~10% margin in `arb_config::limits` — never a guess.

use arb_config::limits::{cu_limit_with_margin, MAX_COMPUTE_UNIT_LIMIT};
use arb_config::program_ids::COMPUTE_BUDGET_PROGRAM;
use solana_program::instruction::Instruction;

const TAG_SET_CU_LIMIT: u8 = 0x02;
const TAG_SET_CU_PRICE: u8 = 0x03;

/// `SetComputeUnitLimit(units)` — the precise CU ceiling for this tx.
pub fn set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(TAG_SET_CU_LIMIT);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM,
        accounts: Vec::new(),
        data,
    }
}

/// `SetComputeUnitPrice(micro_lamports)` — priority fee per CU (priority = limit × price).
pub fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(TAG_SET_CU_PRICE);
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM,
        accounts: Vec::new(),
        data,
    }
}

/// The two ComputeBudget parameters that head every arb tx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputeBudgetParams {
    /// CU ceiling (already margin-applied + clamped).
    pub cu_limit: u32,
    /// Priority fee per CU, in micro-lamports.
    pub cu_price_micro_lamports: u64,
}

impl ComputeBudgetParams {
    /// Size the CU limit from a *measured* simulation figure (LiteSVM/Surfpool/`simulateTx`),
    /// applying the centralized margin and clamping to the protocol ceiling.
    pub fn from_measured(simulated_units: u32, cu_price_micro_lamports: u64) -> Self {
        Self {
            cu_limit: cu_limit_with_margin(simulated_units),
            cu_price_micro_lamports,
        }
    }

    /// The ComputeBudget instructions, in canonical order (limit then price).
    pub fn instructions(&self) -> Vec<Instruction> {
        vec![
            set_compute_unit_limit(self.cu_limit),
            set_compute_unit_price(self.cu_price_micro_lamports),
        ]
    }

    /// True if the CU limit is within the protocol ceiling.
    pub fn is_within_ceiling(&self) -> bool {
        self.cu_limit <= MAX_COMPUTE_UNIT_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cu_limit_encodes_tag_and_le_u32() {
        let ix = set_compute_unit_limit(110_000);
        assert_eq!(ix.program_id, COMPUTE_BUDGET_PROGRAM);
        assert!(ix.accounts.is_empty());
        assert_eq!(ix.data[0], TAG_SET_CU_LIMIT);
        assert_eq!(
            u32::from_le_bytes(ix.data[1..5].try_into().unwrap()),
            110_000
        );
        assert_eq!(ix.data.len(), 5);
    }

    #[test]
    fn set_cu_price_encodes_tag_and_le_u64() {
        let ix = set_compute_unit_price(12_345);
        assert_eq!(ix.data[0], TAG_SET_CU_PRICE);
        assert_eq!(
            u64::from_le_bytes(ix.data[1..9].try_into().unwrap()),
            12_345
        );
        assert_eq!(ix.data.len(), 9);
    }

    #[test]
    fn from_measured_applies_margin_and_emits_two_ixs() {
        let p = ComputeBudgetParams::from_measured(100_000, 50);
        assert_eq!(p.cu_limit, 110_000); // +10% margin
        assert_eq!(p.cu_price_micro_lamports, 50);
        assert!(p.is_within_ceiling());
        let ixs = p.instructions();
        assert_eq!(ixs.len(), 2);
        assert_eq!(ixs[0].data[0], TAG_SET_CU_LIMIT);
        assert_eq!(ixs[1].data[0], TAG_SET_CU_PRICE);
    }
}
