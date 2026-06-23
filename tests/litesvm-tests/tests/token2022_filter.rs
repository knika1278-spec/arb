//! testing-6 (Token-2022 filter half) — exhaustively assert the on-chain mint vetting
//! HARD-REJECTs each of the 7 dangerous extensions and ACCEPTs plain SPL + fee-only /
//! display-only Token-2022. Pure host call into `arb_program::token2022::vet_mint_bytes` (the
//! exact code the program runs); no build-sbf needed, so this stays green on any host.

use arb_program::token2022::vet_mint_bytes;

/// Token-2022 mint TLV region begins at byte 166 (account-type byte at 165).
const TLV_START: usize = 166;

fn mint_with_tlv(entries: &[(u16, &[u8])]) -> Vec<u8> {
    let mut v = vec![0u8; TLV_START];
    for (ty, val) in entries {
        v.extend_from_slice(&ty.to_le_bytes());
        v.extend_from_slice(&(val.len() as u16).to_le_bytes());
        v.extend_from_slice(val);
    }
    v
}

#[test]
fn rejects_all_seven_bad_extensions() {
    let t2022 = &arb_config::program_ids::TOKEN_2022_PROGRAM;
    // ExtensionType discriminants that must HARD-REJECT (mint-relevant subset, plan §8).
    let bad: [(u16, &str); 6] = [
        (3, "MintCloseAuthority"),
        (4, "ConfidentialTransferMint"),
        (6, "DefaultAccountState"),
        (9, "NonTransferable"),
        (12, "PermanentDelegate"),
        (16, "ConfidentialTransferFeeConfig"),
    ];
    for (ty, name) in bad {
        let data = mint_with_tlv(&[(ty, &[0u8; 32])]);
        assert!(
            vet_mint_bytes(t2022, &data).is_err(),
            "extension {name} ({ty}) must be HARD-REJECTed"
        );
    }
    // 7th: TransferHook with a NON-null program id (bytes 32..64 of the value).
    let mut hook = [0u8; 64];
    hook[32] = 1;
    let data = mint_with_tlv(&[(14, &hook)]);
    assert!(
        vet_mint_bytes(t2022, &data).is_err(),
        "TransferHook with a non-null program id must be rejected"
    );
}

#[test]
fn accepts_plain_spl_and_fee_only_and_display_only() {
    let t2022 = &arb_config::program_ids::TOKEN_2022_PROGRAM;
    let spl = &arb_config::program_ids::TOKEN_PROGRAM;
    // Plain SPL mint (classic token program) — always OK.
    assert!(vet_mint_bytes(spl, &[0u8; 82]).is_ok(), "plain SPL mint");
    // Bare Token-2022 mint, no extensions.
    assert!(
        vet_mint_bytes(t2022, &[0u8; 165]).is_ok(),
        "bare T2022 mint"
    );
    // Fee-only Token-2022 (TransferFeeConfig = 1) — the niche we DO support.
    assert!(
        vet_mint_bytes(t2022, &mint_with_tlv(&[(1, &[0u8; 108])])).is_ok(),
        "fee-only T2022 mint"
    );
    // Display-only extensions: InterestBearing (10), ScaledUiAmount (19) — raw amount unchanged.
    assert!(
        vet_mint_bytes(t2022, &mint_with_tlv(&[(10, &[0u8; 40])])).is_ok(),
        "interest-bearing (display-only)"
    );
    assert!(
        vet_mint_bytes(t2022, &mint_with_tlv(&[(19, &[0u8; 40])])).is_ok(),
        "scaled-ui-amount (display-only)"
    );
    // TransferHook with a NULL program id (e.g. PYUSD) — allowed.
    assert!(
        vet_mint_bytes(t2022, &mint_with_tlv(&[(14, &[0u8; 64])])).is_ok(),
        "transfer-hook with null program id"
    );
}
