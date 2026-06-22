//! TEST-ONLY constant-product swap program for the LiteSVM M1-GATE differential.
//!
//! Deployed in LiteSVM AT an allowlisted DEX address (e.g. the Raydium CPMM id) so the
//! `arb-program` trust boundary accepts the CPI, while this controlled implementation lets the
//! gate compare on-chain realized output against the off-chain `arb_math::cpmm::quote_out`
//! mirror. The fee (25/10_000) and the floored CP formula are kept BIT-IDENTICAL to
//! `arb_math::cpmm`. NOT a real venue and NEVER deployed to mainnet.
//!
//! To keep the gate self-contained (LiteSVM ships only native builtins, not SPL Token), the
//! "token" accounts are plain accounts OWNED ON-CHAIN BY THIS PROGRAM carrying the SPL byte
//! layout the arb-program reads (`amount: u64 LE @ 64`). This program moves value by editing
//! that field directly — no SPL Token CPI, no PDA signer. It exercises exactly what the gate
//! needs: the venue's CP arithmetic + the arb-program's snapshot/delta/profit-assert flow.
//!
//! Swap-leg accounts (forwarded by the arb-program adapter as that leg's `remaining_accounts`,
//! all writable, all on-chain-owned by this program):
//! ```text
//! [0] user_source   debited amount_in
//! [1] user_dest     credited `out`
//! [2] pool_src      credited amount_in  (reserve_in)
//! [3] pool_dst      debited `out`       (reserve_out)
//! ```
//! Instruction data = `[discriminator(8)][amount_in: u64 LE][min_out: u64 LE]` — the shape
//! `arb_program::adapters::encode_with_discriminator` produces. The discriminator bytes are
//! not inspected; the arb-program enforces min_out/slippage and the profit assert.
#![allow(unexpected_cfgs)]

use solana_program::account_info::AccountInfo;
use solana_program::entrypoint;
use solana_program::entrypoint::ProgramResult;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

/// MUST match the `arb_math::cpmm` test pools.
const FEE_NUM: u128 = 25;
const FEE_DEN: u128 = 10_000;
const AMOUNT_OFFSET: usize = 64;

entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.len() < 24 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount_in = u64::from_le_bytes(data[8..16].try_into().unwrap());

    let user_source = &accounts[0];
    let user_dest = &accounts[1];
    let pool_src = &accounts[2];
    let pool_dst = &accounts[3];

    // Pre-trade reserves: exactly what the off-chain mirror is quoted against.
    let reserve_in = read_amount(pool_src)?;
    let reserve_out = read_amount(pool_dst)?;
    let out =
        cp_quote_out(reserve_in, reserve_out, amount_in).ok_or(ProgramError::ArithmeticOverflow)?;

    // Move value (this program owns all four accounts on-chain).
    debit(user_source, amount_in)?;
    credit(pool_src, amount_in)?;
    debit(pool_dst, out)?;
    credit(user_dest, out)?;
    Ok(())
}

/// Floored constant-product output, fee on the input first — BIT-IDENTICAL to
/// `arb_math::cpmm::quote_out`.
fn cp_quote_out(reserve_in: u64, reserve_out: u64, amount_in: u64) -> Option<u64> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return Some(0);
    }
    let net = FEE_DEN.checked_sub(FEE_NUM)?;
    let in_after_fee = (amount_in as u128).checked_mul(net)?.checked_div(FEE_DEN)?;
    if in_after_fee == 0 {
        return Some(0);
    }
    let denom = (reserve_in as u128).checked_add(in_after_fee)?;
    let out = (reserve_out as u128)
        .checked_mul(in_after_fee)?
        .checked_div(denom)?;
    let out = out.min((reserve_out as u128).saturating_sub(1));
    u64::try_from(out).ok()
}

fn read_amount(ai: &AccountInfo) -> Result<u64, ProgramError> {
    let data = ai.try_borrow_data()?;
    let bytes = data
        .get(AMOUNT_OFFSET..AMOUNT_OFFSET + 8)
        .ok_or(ProgramError::InvalidAccountData)?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn write_amount(ai: &AccountInfo, v: u64) -> Result<(), ProgramError> {
    let mut data = ai.try_borrow_mut_data()?;
    let slot = data
        .get_mut(AMOUNT_OFFSET..AMOUNT_OFFSET + 8)
        .ok_or(ProgramError::InvalidAccountData)?;
    slot.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn debit(ai: &AccountInfo, amount: u64) -> Result<(), ProgramError> {
    let bal = read_amount(ai)?;
    let next = bal
        .checked_sub(amount)
        .ok_or(ProgramError::InsufficientFunds)?;
    write_amount(ai, next)
}

fn credit(ai: &AccountInfo, amount: u64) -> Result<(), ProgramError> {
    let bal = read_amount(ai)?;
    let next = bal
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    write_amount(ai, next)
}
