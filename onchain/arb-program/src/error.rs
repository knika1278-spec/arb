//! On-chain error codes. We reuse the shared `arb_types::ArbError` numeric ABI so the bot
//! decodes revert reasons against the exact same enum. `to_program_error` maps to
//! `ProgramError::Custom(code)` — the value the runtime surfaces on revert.

use arb_types::ArbError;
use solana_program::program_error::ProgramError;

/// Convert a shared `ArbError` into the `ProgramError::Custom(code)` the runtime returns.
#[inline]
pub fn to_program_error(e: ArbError) -> ProgramError {
    ProgramError::Custom(e.code())
}

/// Ergonomic `Err(arb_err!(Unprofitable))`-style helper.
#[inline]
pub fn err(e: ArbError) -> Result<(), ProgramError> {
    Err(to_program_error(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_custom_code() {
        assert_eq!(
            to_program_error(ArbError::Unprofitable),
            ProgramError::Custom(6000)
        );
        assert_eq!(
            to_program_error(ArbError::UnauthorizedProgram),
            ProgramError::Custom(6001)
        );
    }
}
