//! WSOL dance (txbuilder-4). Every tx whose base/leg asset is native SOL must wrap it into a
//! WSOL token account before the swap CPIs and (for the inventory model) unwrap afterwards —
//! invariant: a leg that "trades SOL" actually moves SPL-wrapped SOL (`NATIVE_MINT`).
//!
//! Canonical sequence placed *inside* the atomic tx:
//! 1. `CreateIdempotent` the WSOL ATA (no-op if it already exists — safe to repeat).
//! 2. System-transfer the exact lamports to wrap into that ATA.
//! 3. `SyncNative` so the token program credits the lamports as WSOL balance.
//! 4. (post) `CloseAccount` back to the authority to unwrap + reclaim rent.
//!
//! WSOL is always owned by the **classic** SPL Token program (not Token-2022), so the dance
//! uses `TOKEN_PROGRAM` and derives the classic ATA.

use arb_config::program_ids::{
    ASSOCIATED_TOKEN_PROGRAM, NATIVE_MINT, SYSTEM_PROGRAM, TOKEN_PROGRAM,
};
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// Associated-token-account discriminator for `CreateIdempotent`.
const ATA_CREATE_IDEMPOTENT: u8 = 1;

/// System program `Transfer` variant index (Borsh u32 enum tag: CreateAccount=0, Assign=1,
/// Transfer=2). Hand-encoded to avoid the deprecated `solana_program::system_instruction`.
const SYSTEM_TRANSFER_TAG: u32 = 2;

/// Build a System-program `Transfer(lamports)` instruction.
fn system_transfer_ix(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&SYSTEM_TRANSFER_TAG.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: SYSTEM_PROGRAM,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

/// Derive the associated token account for `owner`/`mint` under `token_program` (the standard
/// ATA PDA: seeds = `[owner, token_program, mint]`).
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM,
    )
    .0
}

/// Build a `CreateIdempotent` ATA instruction. Idempotent => safe even if the ATA exists, so
/// the hot path never needs an extra existence query.
pub fn create_ata_idempotent_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ata = derive_ata(owner, mint, token_program);
    Instruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![ATA_CREATE_IDEMPOTENT],
    }
}

/// `SyncNative` — make the token program recognize lamports transferred into a WSOL ATA.
fn sync_native_ix(account: &Pubkey) -> Instruction {
    spl_token::instruction::sync_native(&TOKEN_PROGRAM, account)
        .expect("sync_native inputs are always valid")
}

/// `CloseAccount` — unwrap a WSOL ATA back to native SOL at `destination`, reclaiming rent.
fn close_account_ix(account: &Pubkey, destination: &Pubkey, owner: &Pubkey) -> Instruction {
    spl_token::instruction::close_account(&TOKEN_PROGRAM, account, destination, owner, &[])
        .expect("close_account inputs are always valid")
}

/// The instructions of one WSOL dance, split so the builder can place `pre` before the swap
/// CPIs and `post` after them (still inside the same atomic tx).
#[derive(Clone, Debug)]
pub struct WsolPlan {
    /// The classic WSOL ATA for the authority.
    pub wsol_ata: Pubkey,
    /// create-idempotent → fund → sync_native.
    pub pre: Vec<Instruction>,
    /// close (unwrap + reclaim rent).
    pub post: Vec<Instruction>,
}

/// Build the full wrap/unwrap dance: create the ATA, wrap `lamports`, and (post) close it.
///
/// Set `close_after = false` to keep a standing WSOL inventory account (the pre-funded
/// inventory model) — then only `pre` is emitted and the ATA persists across txs.
pub fn wrap_native(authority: &Pubkey, lamports: u64, close_after: bool) -> WsolPlan {
    let wsol_ata = derive_ata(authority, &NATIVE_MINT, &TOKEN_PROGRAM);
    let pre = vec![
        create_ata_idempotent_ix(authority, authority, &NATIVE_MINT, &TOKEN_PROGRAM),
        system_transfer_ix(authority, &wsol_ata, lamports),
        sync_native_ix(&wsol_ata),
    ];
    let post = if close_after {
        vec![close_account_ix(&wsol_ata, authority, authority)]
    } else {
        Vec::new()
    };
    WsolPlan {
        wsol_ata,
        pre,
        post,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    #[test]
    fn ata_derivation_is_deterministic() {
        let owner = key(1);
        let a = derive_ata(&owner, &NATIVE_MINT, &TOKEN_PROGRAM);
        let b = derive_ata(&owner, &NATIVE_MINT, &TOKEN_PROGRAM);
        assert_eq!(a, b);
        // Different owners -> different ATAs.
        assert_ne!(a, derive_ata(&key(2), &NATIVE_MINT, &TOKEN_PROGRAM));
    }

    #[test]
    fn create_idempotent_has_canonical_account_order() {
        let payer = key(1);
        let ix = create_ata_idempotent_ix(&payer, &payer, &NATIVE_MINT, &TOKEN_PROGRAM);
        assert_eq!(ix.program_id, ASSOCIATED_TOKEN_PROGRAM);
        assert_eq!(ix.data, vec![ATA_CREATE_IDEMPOTENT]);
        assert_eq!(ix.accounts.len(), 6);
        assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable); // payer
        assert!(ix.accounts[1].is_writable); // ata
        assert_eq!(ix.accounts[3].pubkey, NATIVE_MINT);
        assert_eq!(ix.accounts[5].pubkey, TOKEN_PROGRAM);
    }

    #[test]
    fn wrap_native_emits_create_fund_sync_then_close() {
        let auth = key(1);
        let plan = wrap_native(&auth, 1_000_000, true);
        assert_eq!(plan.pre.len(), 3);
        assert_eq!(plan.pre[0].program_id, ASSOCIATED_TOKEN_PROGRAM);
        assert_eq!(plan.pre[1].program_id, SYSTEM_PROGRAM); // transfer
        assert_eq!(plan.pre[2].program_id, TOKEN_PROGRAM); // sync_native
        assert_eq!(plan.post.len(), 1);
        assert_eq!(plan.post[0].program_id, TOKEN_PROGRAM); // close
        assert_eq!(
            plan.wsol_ata,
            derive_ata(&auth, &NATIVE_MINT, &TOKEN_PROGRAM)
        );
    }

    #[test]
    fn no_close_when_keeping_inventory() {
        let plan = wrap_native(&key(1), 5, false);
        assert_eq!(plan.pre.len(), 3);
        assert!(plan.post.is_empty());
    }
}
