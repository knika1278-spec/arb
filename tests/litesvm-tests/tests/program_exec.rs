//! Fase-1 real-substrate proof: load the `cargo build-sbf` artifact (`arb_program.so`) into
//! LiteSVM and prove the program actually executes its SBF bytecode. This is the foundation
//! the M1-GATE differential + the revert/trust-boundary unit tests build on (onchain-9/10,
//! testing-5). The `.so` path comes from `ARB_PROGRAM_SO` (set by the WSL test runner); the
//! test self-skips if unset so a plain `cargo test` on a host without build-sbf stays green.

use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

fn arb_so() -> Option<Vec<u8>> {
    let p = std::env::var("ARB_PROGRAM_SO").ok()?;
    std::fs::read(&p).ok()
}

#[test]
fn arb_program_loads_and_executes_in_litesvm() {
    let Some(so) = arb_so() else {
        eprintln!("SKIP arb_program_loads_and_executes_in_litesvm: set ARB_PROGRAM_SO");
        return;
    };
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program(program_id, &so).unwrap();

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    // Malformed (too-short) instruction data: the program unpacks and returns
    // MalformedInstructionData -> the tx must fail. Proves the SBF bytecode ran.
    let ix = Instruction {
        program_id,
        accounts: vec![],
        data: vec![0u8],
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let res = svm.send_transaction(tx);
    assert!(
        res.is_err(),
        "malformed TryArbitrage must revert; program did not reject it: {res:?}"
    );
    eprintln!("arb_program executed in LiteSVM and reverted malformed input as expected");
}
