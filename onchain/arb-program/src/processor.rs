//! `TryArbitrage` processor: the atomic round-trip. Snapshot the base balance, swap leg A
//! (base→intermediate), measure the ACTUAL intermediate delta, swap leg B (intermediate→
//! base) feeding that delta, then the terminal profit-assert. A returned `Err` makes the
//! runtime revert ALL state — the assert is the only real safety net (invariant §2), valid
//! even under `skipPreflight=true`.
//!
//! Account convention (untrusted `remaining_accounts`, strict order):
//! ```text
//! [0] authority            (signer; owns the balance-read ATAs)
//! [1] base_ata             (round-trip source+sink; balance gates the profit-assert)
//! [2] intermediate_ata     (middle token; balance delta carries between legs)
//! [3] leg_a_dex_program    (allowlisted)
//! [4 .. 4+a]    leg A swap accounts (canonical venue order)
//! [4+a]         leg_b_dex_program (allowlisted)
//! [5+a .. 5+a+b] leg B swap accounts
//! ```

use crate::adapters::{invoke_swap, LegContext};
use crate::error::to_program_error;
use crate::instruction::TryArbitrageData;
use crate::state::read_token_amount;
use crate::trust::{verify_authority_signer, verify_balance_account_owner};
use arb_types::ArbError;
use solana_program::account_info::AccountInfo;
use solana_program::entrypoint::ProgramResult;
use solana_program::pubkey::Pubkey;

const IDX_AUTHORITY: usize = 0;
const IDX_BASE_ATA: usize = 1;
const IDX_INTERMEDIATE_ATA: usize = 2;
const FIXED_PREFIX: usize = 3;

pub fn process(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let data = TryArbitrageData::unpack(instruction_data)?;
    let bad = || to_program_error(ArbError::InvalidAccountsList);
    let overflow = || to_program_error(ArbError::ArithmeticOverflow);

    let authority = accounts.get(IDX_AUTHORITY).ok_or_else(bad)?;
    let base_ata = accounts.get(IDX_BASE_ATA).ok_or_else(bad)?;
    let intermediate_ata = accounts.get(IDX_INTERMEDIATE_ATA).ok_or_else(bad)?;

    // Trust boundary: signer + bot-owned balance accounts.
    verify_authority_signer(authority)?;
    verify_balance_account_owner(base_ata, authority.key)?;
    verify_balance_account_owner(intermediate_ata, authority.key)?;

    // Pre-snapshot.
    let pre_base = read_token_amount(base_ata)?;
    let pre_intermediate = read_token_amount(intermediate_ata)?;

    // ---- Leg A: base -> intermediate ----
    let mut cursor = FIXED_PREFIX;
    let leg_a_prog = accounts.get(cursor).ok_or_else(bad)?;
    cursor = cursor.checked_add(1).ok_or_else(bad)?;
    let a_end = cursor
        .checked_add(data.leg_a.account_count as usize)
        .ok_or_else(bad)?;
    let leg_a_accounts = accounts.get(cursor..a_end).ok_or_else(bad)?;
    invoke_swap(
        data.leg_a.dex,
        &LegContext {
            dex_program: leg_a_prog,
            swap_accounts: leg_a_accounts,
            amount_in: data.leg_a.amount_in,
            min_out: data.leg_a.min_out,
        },
    )?;
    cursor = a_end;

    // Measured intermediate delta (ACTUAL balance change — Token-2022 transfer-fee safe).
    let post_intermediate = read_token_amount(intermediate_ata)?;
    let delta = post_intermediate
        .checked_sub(pre_intermediate)
        .ok_or_else(overflow)?;
    if delta < data.leg_a.min_out {
        return Err(to_program_error(ArbError::SlippageExceeded));
    }

    // ---- Leg B: intermediate -> base (fed the measured delta) ----
    let leg_b_prog = accounts.get(cursor).ok_or_else(bad)?;
    cursor = cursor.checked_add(1).ok_or_else(bad)?;
    let b_end = cursor
        .checked_add(data.leg_b.account_count as usize)
        .ok_or_else(bad)?;
    let leg_b_accounts = accounts.get(cursor..b_end).ok_or_else(bad)?;
    let leg_b_in = if data.leg_b.amount_in == 0 {
        delta
    } else {
        data.leg_b.amount_in
    };
    invoke_swap(
        data.leg_b.dex,
        &LegContext {
            dex_program: leg_b_prog,
            swap_accounts: leg_b_accounts,
            amount_in: leg_b_in,
            min_out: data.leg_b.min_out,
        },
    )?;

    // ---- add-2: round-trip closure — the intermediate asset must be fully consumed back to
    // its pre-trade level (leg B spent exactly the measured leg-A delta). A mis-resolved leg B
    // that strands the intermediate yet still grows the base is rejected here, before the
    // profit-assert — the inventory-safety invariant (no base<->intermediate drift; §6/add-2).
    let intermediate_after_b = read_token_amount(intermediate_ata)?;
    if intermediate_after_b != pre_intermediate {
        return Err(to_program_error(ArbError::RouteDoesNotClose));
    }

    // ---- Terminal profit-assert: post_base >= pre_base + min_profit, else revert ALL ----
    let post_base = read_token_amount(base_ata)?;
    let required = pre_base.checked_add(data.min_profit).ok_or_else(overflow)?;
    if post_base < required {
        return Err(to_program_error(ArbError::Unprofitable));
    }
    Ok(())
}
