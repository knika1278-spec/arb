//! testing-6 (trust-boundary half) — the on-chain trust boundary rejects griefer-supplied
//! accounts deterministically, regardless of preflight (LiteSVM executes the program, so the
//! on-chain assert IS the net). Token-2022 mint filtering lives in `token2022_filter.rs`.

mod common;
use common::*;
use solana_sdk::pubkey::Pubkey;

#[test]
fn rejects_non_allowlisted_swap_program() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP rejects_non_allowlisted_swap_program: set env");
        return;
    };
    // The harness is deployed at an allowlisted id, but the leg dex-program account passed to
    // the program is a different, non-allowlisted id -> verify_swap_program must reject pre-CPI.
    let allowed = allowlisted_dex();
    let rogue = Pubkey::new_unique();
    let cfg = GateCfg {
        leg_dex: rogue,
        ..GateCfg::profitable()
    };
    let g = run_roundtrip(&arb, &harness, allowed, &cfg);
    assert!(!g.send_ok, "non-allowlisted swap program must be rejected");
    assert_eq!(
        g.err_code,
        Some(6001),
        "expected UnauthorizedProgram(6001), got {:?}",
        g.err_code
    );
}

#[test]
fn rejects_foreign_owned_balance_account() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP rejects_foreign_owned_balance_account: set env");
        return;
    };
    // The base/intermediate balance accounts carry an owner FIELD that is not the authority —
    // a griefer-substituted account that could fake a profit. verify_balance_account_owner rejects.
    let dex = allowlisted_dex();
    let foreign = Pubkey::new_unique();
    let cfg = GateCfg {
        balance_owner_override: Some(foreign),
        ..GateCfg::profitable()
    };
    let g = run_roundtrip(&arb, &harness, dex, &cfg);
    assert!(!g.send_ok, "foreign-owned balance account must be rejected");
    assert_eq!(
        g.err_code,
        Some(6002),
        "expected UnauthorizedTokenAccountOwner(6002), got {:?}",
        g.err_code
    );
}
