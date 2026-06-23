//! add-2 — inventory round-trip-closure invariant (on-chain half). A mis-resolved leg B that
//! strands the intermediate asset must revert with `RouteDoesNotClose` EVEN WHEN the base
//! balance grows (so the profit assert alone would have passed). This is the inventory-safety
//! property: no base<->intermediate drift. (The signer-side `TxShapeValidator` half is tested in
//! `bot/arb-signer` / `arb-bot`; the off-chain closure check is signer-3.)

mod common;
use common::*;

#[test]
fn stranded_intermediate_reverts_route_does_not_close() {
    let Some((arb, harness)) = artifacts() else {
        eprintln!("SKIP stranded_intermediate_reverts_route_does_not_close: set env");
        return;
    };
    let dex = allowlisted_dex();

    // A profitable edge, but leg B is wired to a SEPARATE pre-funded source, so the real
    // intermediate ATA is never drained back to baseline. Base still grows (would pass the
    // profit assert), yet the closure assert must reject it first.
    let stranded = GateCfg {
        strand_intermediate: true,
        ..GateCfg::profitable()
    };
    let g = run_roundtrip(&arb, &harness, dex, &stranded);
    assert!(
        !g.send_ok,
        "a route that strands the intermediate must revert, but it succeeded"
    );
    assert_eq!(
        g.err_code,
        Some(6010),
        "expected RouteDoesNotClose(6010), got {:?}",
        g.err_code
    );

    // Control: the identical profitable edge that DOES close to the base ATA succeeds — proving
    // it is the stranding (not the economics) that the closure assert rejects.
    let closing = GateCfg {
        strand_intermediate: false,
        ..GateCfg::profitable()
    };
    let g2 = run_roundtrip(&arb, &harness, dex, &closing);
    assert!(
        g2.send_ok,
        "the closing round-trip must succeed, got revert {:?}",
        g2.err_code
    );
}
