//! Tx-builder (txbuilder-1..11): assemble the canonical atomic-arb transaction and prove it
//! is within every hard limit BEFORE it reaches the signer.
//!
//! Pipeline (consumed by the signer → executor): a `SizedTrade` + resolved route accounts →
//! `[ComputeBudget, WSOL-wrap, TryArbitrage, WSOL-close]` on a pre-warmed ALT → a measured,
//! validated [`BuiltTxPlan`]. The whole module is openssl-free (host-green): it reuses the
//! on-chain `arb-program` for the instruction ABI + Token-2022 vetting, hand-encodes the
//! ComputeBudget ixs, and computes account-locks / serialized-size from the v0 wire layout.
//!
//! The v0 message assembler ([`message::compile_v0_message`]) compiles the instruction list +
//! pre-warmed ALTs + a recent blockhash into a `VersionedMessage` (txbuilder-5). SIGNING and
//! serialization remain the signer/executor concern — the hot key is never touched here. This
//! module produces the ordered instruction list, the ALT set, the signer set, the exact
//! [`LimitReport`], and the unsigned v0 message.

pub mod alt;
pub mod compute;
pub mod error;
pub mod layout;
pub mod limits;
pub mod message;
pub mod preflight;
pub mod sellsim;
pub mod tip;
pub mod token2022;
pub mod vet;
pub mod whirlpool;
pub mod wsol;

pub use compute::ComputeBudgetParams;
pub use error::TxBuilderError;
pub use layout::{build_arb_instruction, LegAccounts};
pub use limits::{measure, AltView, LimitReport};
pub use message::compile_v0_message;
pub use preflight::{
    decode_revert, evaluate as evaluate_preflight, preflight_simulate, PreflightOk, SimOutcome,
    SimulateRpc,
};
pub use sellsim::{
    classify_sell, run_sell_sim_gate, SellSimError, SellSimPolicy, SellSimResult, SellSimulator,
    SellVerdict,
};
pub use tip::{build_capped_tip_ix, jito_tip_ix, tip_cap};
pub use token2022::Token2022Filter;
pub use vet::{vet_route, RouteVetInput};
pub use whirlpool::{
    oracle_pda, resolve_swap_accounts, start_tick_index, tick_array_pda, WhirlpoolResolveError,
    WhirlpoolSwapAccounts,
};
pub use wsol::{derive_ata, wrap_native, WsolPlan};

use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// Static builder configuration (invariant accounts + pricing).
#[derive(Clone, Copy, Debug)]
pub struct TxBuilderConfig {
    /// Deployed `arb-program` id (set post-deploy; placeholder until `onchain-13`).
    pub arb_program_id: Pubkey,
    /// The bot authority — fee payer + signer + owner of the balance-read ATAs.
    pub authority: Pubkey,
    /// Priority fee per CU (micro-lamports). Sized by the executor's tip/priority model.
    pub cu_price_micro_lamports: u64,
    /// Whether the base asset is native SOL (=> emit the WSOL dance).
    pub base_is_wsol: bool,
    /// Whether to unwrap (close the WSOL ATA) after the round trip, or keep standing inventory.
    pub close_wsol_after: bool,
}

/// A fully-resolved route ready to assemble: both legs' accounts, the balance-read ATAs, the
/// sizing amounts, and the ALTs to compress it under the byte cap.
#[derive(Clone, Debug)]
pub struct ArbRoute {
    pub leg_a: LegAccounts,
    pub leg_b: LegAccounts,
    /// Profit-assert balance-read accounts (base ATA first, then intermediate ATA), writable.
    pub balance_read: Vec<AccountMeta>,
    /// Leg-A input amount (leg B chains off the measured delta).
    pub size_in: u64,
    /// Costs-inclusive base-asset profit floor (dec-3: one shared definition).
    pub min_profit: u64,
    /// Measured CU from preflight simulation (margin applied internally).
    pub measured_cu: u32,
    /// ALTs to attach as `(table, addresses-it-holds)`.
    pub alts: Vec<(Pubkey, Vec<Pubkey>)>,
}

/// The assembled, measured, hard-limit-validated plan handed to the signer.
#[derive(Clone, Debug)]
pub struct BuiltTxPlan {
    /// Canonical instruction order: ComputeBudget → WSOL-pre → TryArbitrage → WSOL-post.
    pub instructions: Vec<Instruction>,
    /// ALTs the executor must attach when finalizing the v0 message.
    pub alt_tables: Vec<Pubkey>,
    /// Required signers (authority only for M1).
    pub signers: Vec<Pubkey>,
    /// The measured budget; already `validate()`-passed.
    pub report: LimitReport,
}

/// Assemble + measure + hard-limit-validate the atomic-arb transaction plan.
///
/// Does NOT sign or attach a blockhash (signer/executor seam). The Jito tip instruction is a
/// Fase-2 addition (`txbuilder-13`) inserted INSIDE this list so a failed arb pays no tip.
pub fn build_arb_tx(
    cfg: &TxBuilderConfig,
    route: &ArbRoute,
) -> Result<BuiltTxPlan, TxBuilderError> {
    let compute =
        ComputeBudgetParams::from_measured(route.measured_cu, cfg.cu_price_micro_lamports);
    let mut instructions = compute.instructions();

    let wsol = if cfg.base_is_wsol {
        Some(wrap_native(
            &cfg.authority,
            route.size_in,
            cfg.close_wsol_after,
        ))
    } else {
        None
    };
    if let Some(w) = &wsol {
        instructions.extend_from_slice(&w.pre);
    }

    let arb_ix = build_arb_instruction(
        cfg.arb_program_id,
        cfg.authority,
        route.size_in,
        route.min_profit,
        &route.leg_a,
        &route.leg_b,
        &route.balance_read,
    )?;
    instructions.push(arb_ix);

    if let Some(w) = &wsol {
        instructions.extend_from_slice(&w.post);
    }

    let alt_views: Vec<AltView> = route
        .alts
        .iter()
        .map(|(t, addrs)| AltView {
            table: *t,
            addresses: addrs.as_slice(),
        })
        .collect();

    let report = measure(
        &cfg.authority,
        &[],
        &instructions,
        &alt_views,
        compute.cu_limit,
    );
    report.validate()?;

    Ok(BuiltTxPlan {
        instructions,
        alt_tables: route.alts.iter().map(|(t, _)| *t).collect(),
        signers: vec![cfg.authority],
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{DexKind, SwapDir};

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn leg(dex: DexKind, dir: SwapDir, start: u8, n: u8) -> LegAccounts {
        LegAccounts {
            dex,
            dir,
            min_out: 100,
            metas: (0..n)
                .map(|i| AccountMeta::new_readonly(key(start + i), false))
                .collect(),
        }
    }

    fn route() -> ArbRoute {
        let leg_a = leg(DexKind::RaydiumCpmm, SwapDir::AtoB, 40, 9);
        let leg_b = leg(DexKind::OrcaWhirlpool, SwapDir::BtoA, 60, 11);
        // ALT covers the pool accounts so the byte cap holds.
        let mut alt_addrs: Vec<Pubkey> = (40..49u8).map(key).collect();
        alt_addrs.extend((60..71u8).map(key));
        ArbRoute {
            leg_a,
            leg_b,
            balance_read: vec![
                AccountMeta::new(key(10), false),
                AccountMeta::new(key(11), false),
            ],
            size_in: 1_000_000,
            min_profit: 5_000,
            measured_cu: 180_000,
            alts: vec![(key(200), alt_addrs)],
        }
    }

    #[test]
    fn builds_validated_plan_with_wsol_dance() {
        let cfg = TxBuilderConfig {
            arb_program_id: key(123),
            authority: key(1),
            cu_price_micro_lamports: 50,
            base_is_wsol: true,
            close_wsol_after: true,
        };
        let plan = build_arb_tx(&cfg, &route()).expect("within limits");

        // ComputeBudget(2) + WSOL pre(3) + TryArbitrage(1) + WSOL post(1) = 7.
        assert_eq!(plan.instructions.len(), 7);
        assert_eq!(plan.signers, vec![key(1)]);
        assert_eq!(plan.report.compute_units, 198_000); // 180k + 10%
        plan.report.validate().expect("report passes");
        assert!(plan.report.serialized_len <= arb_config::limits::TX_SIZE_LIMIT_BYTES);
        assert!(plan.report.account_locks <= arb_config::limits::MAX_TX_ACCOUNT_LOCKS);
        // Pool accounts were ALT-resolved, not crammed into the static keys.
        assert!(plan.report.num_alt_loaded >= 20);
    }

    #[test]
    fn no_wsol_dance_when_base_is_not_sol() {
        let cfg = TxBuilderConfig {
            arb_program_id: key(123),
            authority: key(1),
            cu_price_micro_lamports: 0,
            base_is_wsol: false,
            close_wsol_after: false,
        };
        let plan = build_arb_tx(&cfg, &route()).unwrap();
        // ComputeBudget(2) + TryArbitrage(1) = 3.
        assert_eq!(plan.instructions.len(), 3);
    }
}
