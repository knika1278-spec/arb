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
use crate::instruction::{
    TryArbitrageData, TryArbitrageNData, TAG_TRY_ARBITRAGE, TAG_TRY_ARBITRAGE_N,
};
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

/// Instruction dispatcher: tag 0 = the proven 2-leg round-trip, tag 1 = the N-leg (triangle+)
/// cycle (onchain-20, gated by `M1-GATE-EXT`/`testing-11`).
pub fn process(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match instruction_data.first().copied() {
        Some(TAG_TRY_ARBITRAGE) => process_two_leg(accounts, instruction_data),
        Some(TAG_TRY_ARBITRAGE_N) => process_n_leg(accounts, instruction_data),
        _ => Err(to_program_error(ArbError::MalformedInstructionData)),
    }
}

/// The 2-leg round-trip (tag 0) — the M1-GATE-proven control flow, unchanged.
fn process_two_leg(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
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

/// onchain-20: the N-leg (triangle+) cycle. Generalizes `process_two_leg` to a cycle of
/// `leg_count` swaps `base → t1 → … → t_{N-1} → base` measured by ACTUAL balance deltas
/// (Token-2022 safe), ending with the same terminal profit-assert. Any `Err` reverts ALL state.
///
/// Account convention (untrusted `remaining_accounts`, strict order):
/// ```text
/// [0]              authority (signer; owns every cycle ATA)
/// [1 .. 1+N]       the N cycle ATAs; ata[0] = base, ata[i] = token i (balance gates the asserts)
/// then per leg i:  [dex_program_i (allowlisted)] [leg_i swap accounts (account_count_i)]
/// ```
/// Leg `i` swaps `ata[i] → ata[(i+1) mod N]`; the last leg returns to `ata[0]` (base). For any
/// non-first leg, `amount_in == 0` means "feed the measured delta the previous leg produced".
fn process_n_leg(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let data = TryArbitrageNData::unpack(instruction_data)?;
    let bad = || to_program_error(ArbError::InvalidAccountsList);
    let overflow = || to_program_error(ArbError::ArithmeticOverflow);

    let n = data.leg_count as usize;
    // [0] authority, [1..1+n] the n cycle ATAs (ata[0] = base).
    let authority = accounts.get(IDX_AUTHORITY).ok_or_else(bad)?;
    verify_authority_signer(authority)?;
    let atas = accounts.get(1..1 + n).ok_or_else(bad)?;
    for ata in atas {
        verify_balance_account_owner(ata, authority.key)?;
    }
    let base_ata = &atas[0];
    let pre_base = read_token_amount(base_ata)?;

    // add-2 (N-leg): snapshot each intermediate ATA (ata[1..n]) so we can prove the cycle
    // closes — every intermediate asset must return to its pre-trade level (no stranded
    // inventory). ata[0] (base) is gated by the terminal profit-assert instead.
    let mut pre_inter = [0u64; crate::instruction::MAX_LEGS];
    for (slot, ata) in pre_inter.iter_mut().zip(atas.iter()).skip(1) {
        *slot = read_token_amount(ata)?;
    }

    // Walk each leg, chaining the ACTUAL measured output delta into the next leg's input.
    let mut cursor = 1usize.checked_add(n).ok_or_else(bad)?;
    let mut carry = 0u64;
    for (i, leg) in data.legs().iter().enumerate() {
        let out_ata = &atas[(i + 1) % n];
        let dex_prog = accounts.get(cursor).ok_or_else(bad)?;
        cursor = cursor.checked_add(1).ok_or_else(bad)?;
        let end = cursor
            .checked_add(leg.account_count as usize)
            .ok_or_else(bad)?;
        let leg_accounts = accounts.get(cursor..end).ok_or_else(bad)?;
        cursor = end;

        // First leg uses the explicit sizing input; later legs default to the measured carry.
        let amount_in = if i == 0 || leg.amount_in != 0 {
            leg.amount_in
        } else {
            carry
        };

        let pre_out = read_token_amount(out_ata)?;
        invoke_swap(
            leg.dex,
            &LegContext {
                dex_program: dex_prog,
                swap_accounts: leg_accounts,
                amount_in,
                min_out: leg.min_out,
            },
        )?;
        let post_out = read_token_amount(out_ata)?;
        let delta = post_out.checked_sub(pre_out).ok_or_else(overflow)?;
        if delta < leg.min_out {
            return Err(to_program_error(ArbError::SlippageExceeded));
        }
        carry = delta;
    }

    // ---- add-2 (N-leg) closure: every intermediate must return to its pre-trade level ----
    for (pre, ata) in pre_inter.iter().zip(atas.iter()).skip(1) {
        if read_token_amount(ata)? != *pre {
            return Err(to_program_error(ArbError::RouteDoesNotClose));
        }
    }

    // ---- Terminal profit-assert on the base ATA: post_base >= pre_base + min_profit ----
    let post_base = read_token_amount(base_ata)?;
    let required = pre_base.checked_add(data.min_profit).ok_or_else(overflow)?;
    if post_base < required {
        return Err(to_program_error(ArbError::Unprofitable));
    }
    Ok(())
}
