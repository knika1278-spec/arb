//! Core v0 message assembler (txbuilder-5). Compiles the validated instruction list into a v0
//! `VersionedMessage` with the pre-warmed ALTs and a recent blockhash. This is the last piece
//! before the signer: it attaches the blockhash and resolves account keys through the ALTs, but
//! does NOT sign or serialize — the hot key is the signer module's exclusive concern (§6).

use crate::txbuilder::TxBuilderError;
use solana_message::{v0, AddressLookupTableAccount, VersionedMessage};
use solana_program::hash::Hash;
use solana_program::instruction::Instruction;
use solana_pubkey::Pubkey;

/// Compile `instructions` (the canonical ComputeBudget -> WSOL -> TryArbitrage -> WSOL order)
/// into a v0 message: the ALTs compress the account keys under the 1232-byte cap, and the
/// recent blockhash binds the tx to a slot window. Unsigned by design (signer seam).
///
/// `alt_accounts` are the on-chain ALT contents (key + the addresses each table holds) the
/// executor fetched for the plan's `alt_tables`; only the pre-warmed set is passed (invariant
/// §4: never extend-then-use in the same slot).
pub fn compile_v0_message(
    payer: &Pubkey,
    instructions: &[Instruction],
    alt_accounts: &[AddressLookupTableAccount],
    recent_blockhash: Hash,
) -> Result<VersionedMessage, TxBuilderError> {
    let msg = v0::Message::try_compile(payer, instructions, alt_accounts, recent_blockhash)
        .map_err(|e| TxBuilderError::MessageCompile(e.to_string()))?;
    Ok(VersionedMessage::V0(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::instruction::AccountMeta;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn compiles_v0_and_resolves_writable_pool_through_alt() {
        let payer = key(1);
        let program = key(9);
        let pool = key(40); // writable, non-signer, present in the ALT -> resolved dynamically
        let ix = Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(payer, true), AccountMeta::new(pool, false)],
            data: vec![0u8],
        };
        let alt = AddressLookupTableAccount {
            key: key(200),
            addresses: vec![pool],
        };

        let msg = compile_v0_message(&payer, &[ix], &[alt], Hash::default()).unwrap();
        match msg {
            VersionedMessage::V0(m) => {
                // The pre-warmed ALT was actually used to look the pool key up.
                assert_eq!(m.address_table_lookups.len(), 1);
                assert_eq!(m.address_table_lookups[0].account_key, key(200));
                // The payer stays a static signer key (never resolved through an ALT).
                assert_eq!(m.account_keys[0], payer);
            }
            VersionedMessage::Legacy(_) => panic!("expected a v0 message"),
        }
    }

    #[test]
    fn compiles_without_alts() {
        let payer = key(1);
        let ix = Instruction {
            program_id: key(9),
            accounts: vec![AccountMeta::new(payer, true)],
            data: vec![],
        };
        let msg = compile_v0_message(&payer, &[ix], &[], Hash::default()).unwrap();
        assert!(matches!(msg, VersionedMessage::V0(_)));
    }
}
