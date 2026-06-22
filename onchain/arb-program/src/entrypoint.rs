//! SBF entrypoint. Gated by `no-entrypoint` so the crate can also be linked as a plain
//! library (tests, rounding-mirror, the bot) without exporting the program symbol.
#![cfg(not(feature = "no-entrypoint"))]

use solana_program::account_info::AccountInfo;
use solana_program::entrypoint::ProgramResult;
use solana_program::pubkey::Pubkey;

solana_program::entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    crate::processor::process(program_id, accounts, instruction_data)
}
