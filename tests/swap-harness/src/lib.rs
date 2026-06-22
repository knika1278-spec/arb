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

/// Default fee when a pool account carries no explicit fee (bytes 165..181). MUST match
/// `arb_math::cpmm` / `common::DEFAULT_FEE_*` so the rounding mirror stays bit-exact.
const DEFAULT_FEE_NUM: u128 = 25;
const DEFAULT_FEE_DEN: u128 = 10_000;
const AMOUNT_OFFSET: usize = 64;
/// Optional per-pool fee the rounding-mirror fuzz writes into the pool_src account so it can
/// sweep fee (not just reserves). u64 LE each; absent/zero-den ⇒ default above.
const FEE_NUM_OFFSET: usize = 165;
const FEE_DEN_OFFSET: usize = 173;
/// Optional Token-2022 receipt transfer fee on a `user_dest` account (bps u16 @181, max u64 @183).
const RECV_BPS_OFFSET: usize = 181;
const RECV_MAX_OFFSET: usize = 183;

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

    // Pre-trade reserves + fee: exactly what the off-chain mirror is quoted against.
    let reserve_in = read_amount(pool_src)?;
    let reserve_out = read_amount(pool_dst)?;
    let (fee_num, fee_den) = read_fee(pool_src);
    let out = cp_quote_out(reserve_in, reserve_out, fee_num, fee_den, amount_in)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    // Move value (this program owns all four accounts on-chain). A Token-2022 receipt fee tagged
    // on `user_dest` is skimmed from the credited amount — the fee "vanishes" to the mint's
    // withheld-fees exactly as spl-token-2022 does, so the arb-program measures the NET delta.
    let (recv_bps, recv_max) = read_recv_fee(user_dest);
    let net_out = out
        .checked_sub(transfer_fee(out, recv_bps, recv_max))
        .ok_or(ProgramError::ArithmeticOverflow)?;
    debit(user_source, amount_in)?;
    credit(pool_src, amount_in)?;
    debit(pool_dst, out)?;
    credit(user_dest, net_out)?;
    Ok(())
}

/// Floored constant-product output, fee on the input first — BIT-IDENTICAL to
/// `arb_math::cpmm::quote_out`.
fn cp_quote_out(
    reserve_in: u64,
    reserve_out: u64,
    fee_num: u128,
    fee_den: u128,
    amount_in: u64,
) -> Option<u64> {
    if fee_den == 0 || fee_num > fee_den {
        return None;
    }
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return Some(0);
    }
    let net = fee_den.checked_sub(fee_num)?;
    let in_after_fee = (amount_in as u128).checked_mul(net)?.checked_div(fee_den)?;
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

/// Per-pool fee `(num, den)` from bytes 165..181 of the reserve-in account; default 25/10_000
/// when absent or invalid (so the original 165-byte gate accounts keep the Raydium fee).
fn read_fee(ai: &AccountInfo) -> (u128, u128) {
    if let Ok(data) = ai.try_borrow_data() {
        if data.len() >= FEE_DEN_OFFSET + 8 {
            let fnum =
                u64::from_le_bytes(data[FEE_NUM_OFFSET..FEE_NUM_OFFSET + 8].try_into().unwrap())
                    as u128;
            let fden =
                u64::from_le_bytes(data[FEE_DEN_OFFSET..FEE_DEN_OFFSET + 8].try_into().unwrap())
                    as u128;
            if fden != 0 && fnum <= fden {
                return (fnum, fden);
            }
        }
    }
    (DEFAULT_FEE_NUM, DEFAULT_FEE_DEN)
}

/// Token-2022 receipt transfer fee `(bps, max)` tagged on a `user_dest` account at bytes
/// 181..191; `(0, 0)` (no skim) when absent.
fn read_recv_fee(ai: &AccountInfo) -> (u16, u64) {
    if let Ok(data) = ai.try_borrow_data() {
        if data.len() >= RECV_MAX_OFFSET + 8 {
            let bps = u16::from_le_bytes(
                data[RECV_BPS_OFFSET..RECV_BPS_OFFSET + 2]
                    .try_into()
                    .unwrap(),
            );
            let max = u64::from_le_bytes(
                data[RECV_MAX_OFFSET..RECV_MAX_OFFSET + 8]
                    .try_into()
                    .unwrap(),
            );
            return (bps, max);
        }
    }
    (0, 0)
}

/// Ceiling transfer fee capped at `max` — bit-identical to `arb_math::fees::calculate_fee`.
fn transfer_fee(amount: u64, bps: u16, max: u64) -> u64 {
    if bps == 0 || amount == 0 {
        return 0;
    }
    let numerator = (amount as u128).saturating_mul(bps as u128);
    let raw = numerator.saturating_add(10_000 - 1) / 10_000; // ceil
    raw.min(max as u128) as u64
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
