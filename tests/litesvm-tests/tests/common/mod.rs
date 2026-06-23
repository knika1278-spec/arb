//! Shared LiteSVM M1-GATE harness — single-sourced account-order builders + the generalized
//! round-trip runner (both directions, per-pool fee, CU capture) + deterministic fuzzing +
//! off-chain mirror. Every gate test file (`m1_gate`, `rounding_mirror_fuzz`, `litesvm_unit`,
//! `trust_boundary`, `closure`, `cu_budget`) drives the SAME builders so the on-chain account
//! ordering can never drift between tests (onchain-9: "single-source the account-order builders").
//!
//! The harness "token" accounts carry the SPL byte layout the arb-program reads (`amount`@64);
//! pool accounts optionally carry a per-pool fee at bytes 165..181 that the swap-harness reads
//! (so the rounding-mirror fuzz can sweep fee, not just reserves) — absent ⇒ the harness uses
//! the default 25/10_000. Real-venue fees + sqrt-price are proven separately on Surfpool.
#![allow(dead_code)] // each test file uses a subset of this surface

use arb_math::{CpmmReserves, RoundTrip};
use arb_program::instruction::{LegDescriptor, TryArbitrageData};
use arb_types::{DexKind, SwapDir};
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

/// Raydium-style default fee the harness falls back to when a pool account carries no fee.
pub const DEFAULT_FEE_NUM: u64 = 25;
pub const DEFAULT_FEE_DEN: u64 = 10_000;

/// Read a build-sbf artifact whose path is in env var `var`.
pub fn so(var: &str) -> Option<Vec<u8>> {
    std::fs::read(std::env::var(var).ok()?).ok()
}

/// `(arb_program.so, swap_harness.so)` from `ARB_PROGRAM_SO` / `SWAP_HARNESS_SO`, or `None`
/// so a host `cargo test` without build-sbf self-skips and stays green.
pub fn artifacts() -> Option<(Vec<u8>, Vec<u8>)> {
    Some((so("ARB_PROGRAM_SO")?, so("SWAP_HARNESS_SO")?))
}

/// The i-th Wave-1 allowlisted DEX id (0=Raydium CPMM, 1=Orca Whirlpool, 2=PumpSwap). Rebuilt
/// from bytes to dodge any cross-crate Pubkey type duplication.
pub fn allowlisted_dex_n(i: usize) -> Pubkey {
    Pubkey::new_from_array(arb_config::WAVE1_DEX_ALLOWLIST[i].to_bytes())
}
pub fn allowlisted_dex() -> Pubkey {
    allowlisted_dex_n(0)
}

/// SPL/Token-2022-layout account (owner@32, amount@64), owned on-chain by `onchain_owner` so
/// the harness program may edit the balance directly. 165-byte form (no harness fee bytes).
pub fn token_account(owner_field: &Pubkey, amount: u64, onchain_owner: &Pubkey) -> Account {
    build_token_account(owner_field, amount, onchain_owner, None)
}

/// Same, but appends a per-pool fee (num, den) at bytes 165..181 for the harness to read.
pub fn token_account_fee(
    owner_field: &Pubkey,
    amount: u64,
    onchain_owner: &Pubkey,
    fee_num: u64,
    fee_den: u64,
) -> Account {
    build_token_account(owner_field, amount, onchain_owner, Some((fee_num, fee_den)))
}

fn build_token_account(
    owner_field: &Pubkey,
    amount: u64,
    onchain_owner: &Pubkey,
    fee: Option<(u64, u64)>,
) -> Account {
    let len = if fee.is_some() { 181 } else { 165 };
    let mut data = vec![0u8; len];
    data[32..64].copy_from_slice(owner_field.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1; // AccountState::Initialized
    if let Some((fnum, fden)) = fee {
        data[165..173].copy_from_slice(&fnum.to_le_bytes());
        data[173..181].copy_from_slice(&fden.to_le_bytes());
    }
    Account {
        lamports: 1_000_000,
        data,
        owner: *onchain_owner,
        executable: false,
        rent_epoch: 0,
    }
}

/// Token account carrying a Token-2022-style transfer fee on RECEIPT (`bps`, `max`) at bytes
/// 181..191. The swap-harness skims this fee when crediting the account as a swap `user_dest`,
/// so the round-trip exercises the processor's measure-actual-delta path (the fee skims the
/// received intermediate). Mirrors `arb_math::fees::calculate_fee` (ceil, capped).
pub fn token_account_t22(
    owner_field: &Pubkey,
    amount: u64,
    onchain_owner: &Pubkey,
    recv_bps: u16,
    recv_max: u64,
) -> Account {
    let mut data = vec![0u8; 191];
    data[32..64].copy_from_slice(owner_field.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    data[181..183].copy_from_slice(&recv_bps.to_le_bytes());
    data[183..191].copy_from_slice(&recv_max.to_le_bytes());
    Account {
        lamports: 1_000_000,
        data,
        owner: *onchain_owner,
        executable: false,
        rent_epoch: 0,
    }
}

/// Extract a program `Custom(code)` from a transaction error (for asserting exact ArbError codes).
pub fn custom_code(err: &TransactionError) -> Option<u32> {
    match err {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => Some(*c),
        _ => None,
    }
}

/// Current SPL `amount` (balance@64) of a token account in the SVM (0 if absent). After a
/// reverted tx LiteSVM restores pre-trade state, so this reads the unchanged balance.
pub fn token_balance(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key)
        .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
        .unwrap_or(0)
}

/// One pool leg: reserves (token A / token B), fee, and the swap direction.
#[derive(Clone, Copy)]
pub struct PoolCfg {
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub fee_num: u64,
    pub fee_den: u64,
    pub dir: SwapDir,
}

impl PoolCfg {
    /// 25 bps CP pool in the given direction.
    pub fn new(reserve_a: u64, reserve_b: u64, dir: SwapDir) -> Self {
        Self {
            reserve_a,
            reserve_b,
            fee_num: DEFAULT_FEE_NUM,
            fee_den: DEFAULT_FEE_DEN,
            dir,
        }
    }
    pub fn with_fee(mut self, fee_num: u64, fee_den: u64) -> Self {
        self.fee_num = fee_num;
        self.fee_den = fee_den;
        self
    }
    /// `(reserve_in, reserve_out)` for this leg's direction (what the harness pool accounts hold).
    pub fn oriented(&self) -> (u64, u64) {
        match self.dir {
            SwapDir::AtoB => (self.reserve_a, self.reserve_b),
            SwapDir::BtoA => (self.reserve_b, self.reserve_a),
        }
    }
    pub fn reserves(&self) -> CpmmReserves {
        CpmmReserves::new(self.reserve_a, self.reserve_b, self.fee_num, self.fee_den)
    }
}

/// A full round-trip scenario. Negative-test knobs default to "off" via the constructors.
#[derive(Clone, Copy)]
pub struct GateCfg {
    pub pool_a: PoolCfg,
    pub pool_b: PoolCfg,
    pub base_funding: u64,
    pub delta_in: u64,
    pub min_profit: u64,
    /// DEX program id passed to the program for BOTH legs. Use a non-allowlisted id to drive the
    /// trust-boundary rejection; defaults to the allowlisted id the harness is deployed at.
    pub leg_dex: Pubkey,
    /// Override the OWNER field of the bot-owned balance accounts (base + intermediate) to
    /// simulate a griefer-supplied account not owned by the authority (trust-boundary negative).
    pub balance_owner_override: Option<Pubkey>,
    /// Wire leg B to a separate pre-funded source so the real intermediate ATA is left stranded
    /// (add-2 closure negative): base still grows, but the intermediate never returns to baseline.
    pub strand_intermediate: bool,
    /// Tag the intermediate ATA with a Token-2022 receipt transfer fee `(bps, max)` so leg A's
    /// credit is skimmed — exercising the processor's measure-actual-delta path. The off-chain
    /// prediction must apply `arb_math::fees::amount_after_fee` between the legs to match.
    pub inter_recv_fee: Option<(u16, u64)>,
}

impl GateCfg {
    /// The canonical profitable two-pool edge from the arb_math example, both legs AtoB.
    pub fn profitable() -> Self {
        Self {
            pool_a: PoolCfg::new(1_000_000, 2_000_000, SwapDir::AtoB),
            pool_b: PoolCfg::new(2_000_000, 1_100_000, SwapDir::AtoB),
            base_funding: 1_000_000,
            delta_in: 5_000,
            min_profit: 0,
            leg_dex: allowlisted_dex(),
            balance_owner_override: None,
            strand_intermediate: false,
            inter_recv_fee: None,
        }
    }
    pub fn with_delta(mut self, d: u64) -> Self {
        self.delta_in = d;
        self
    }
    pub fn with_min_profit(mut self, m: u64) -> Self {
        self.min_profit = m;
        self
    }
}

/// Outcome of one round-trip on LiteSVM.
pub struct Gate {
    pub send_ok: bool,
    /// Final base-asset amount produced by the round-trip (success only).
    pub realized_final: Option<u64>,
    /// Off-chain `arb_math` prediction of the same quantity (always computed).
    pub predicted_final: Option<u64>,
    /// Compute units the tx consumed (success only).
    pub cu: Option<u64>,
    /// The program `Custom(code)` on revert, if any.
    pub err_code: Option<u32>,
    /// Base-ATA balance after the tx (== pre-trade funding when the tx reverted).
    pub base_balance: u64,
    /// Intermediate-ATA balance after the tx (== 0 when a clean round-trip reverted).
    pub inter_balance: u64,
    pub logs: String,
}

/// Off-chain predicted profit (final - delta) as a signed integer.
pub fn predicted_profit(cfg: &GateCfg) -> Option<i128> {
    let f = predicted_final(cfg)?;
    Some(f as i128 - cfg.delta_in as i128)
}

pub fn predicted_final(cfg: &GateCfg) -> Option<u64> {
    let rt = RoundTrip::new(
        cfg.pool_a.reserves(),
        cfg.pool_a.dir,
        cfg.pool_b.reserves(),
        cfg.pool_b.dir,
    );
    rt.realized_out(cfg.delta_in)
}

/// Build a fresh SVM, set up the base/intermediate ATAs + two pools (oriented by direction,
/// fee-tagged), run the round-trip, and return realized-vs-predicted + CU + revert code.
/// `harness_dex_id` is the (allowlisted) id the swap-harness is deployed at.
pub fn run_roundtrip(
    arb_so: &[u8],
    harness_so: &[u8],
    harness_dex_id: Pubkey,
    cfg: &GateCfg,
) -> Gate {
    let mut svm = LiteSVM::new();
    let arb_id = Pubkey::new_unique();
    svm.add_program(arb_id, arb_so).unwrap();
    svm.add_program(harness_dex_id, harness_so).unwrap();

    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 1_000_000_000).unwrap();

    let oc = harness_dex_id; // on-chain owner of the synthetic token accounts
    let bal_owner = cfg
        .balance_owner_override
        .unwrap_or_else(|| authority.pubkey());

    let base_ata = Pubkey::new_unique();
    let inter_ata = Pubkey::new_unique();
    let pa_in = Pubkey::new_unique();
    let pa_out = Pubkey::new_unique();
    let pb_in = Pubkey::new_unique();
    let pb_out = Pubkey::new_unique();

    svm.set_account(base_ata, token_account(&bal_owner, cfg.base_funding, &oc))
        .unwrap();
    let inter_acct = match cfg.inter_recv_fee {
        Some((bps, max)) => token_account_t22(&bal_owner, 0, &oc, bps, max),
        None => token_account(&bal_owner, 0, &oc),
    };
    svm.set_account(inter_ata, inter_acct).unwrap();

    let (pa_in_amt, pa_out_amt) = cfg.pool_a.oriented();
    let (pb_in_amt, pb_out_amt) = cfg.pool_b.oriented();
    svm.set_account(
        pa_in,
        token_account_fee(
            &authority.pubkey(),
            pa_in_amt,
            &oc,
            cfg.pool_a.fee_num,
            cfg.pool_a.fee_den,
        ),
    )
    .unwrap();
    svm.set_account(pa_out, token_account(&authority.pubkey(), pa_out_amt, &oc))
        .unwrap();
    svm.set_account(
        pb_in,
        token_account_fee(
            &authority.pubkey(),
            pb_in_amt,
            &oc,
            cfg.pool_b.fee_num,
            cfg.pool_b.fee_den,
        ),
    )
    .unwrap();
    svm.set_account(pb_out, token_account(&authority.pubkey(), pb_out_amt, &oc))
        .unwrap();

    // Leg-B source: the real intermediate ATA, unless we are deliberately stranding it.
    let legb_source = if cfg.strand_intermediate {
        let alt = Pubkey::new_unique();
        svm.set_account(alt, token_account(&authority.pubkey(), pb_in_amt, &oc))
            .unwrap();
        alt
    } else {
        inter_ata
    };

    let data = TryArbitrageData {
        min_profit: cfg.min_profit,
        leg_a: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: cfg.pool_a.dir,
            account_count: 4,
            amount_in: cfg.delta_in,
            min_out: 0,
        },
        leg_b: LegDescriptor {
            dex: DexKind::RaydiumCpmm,
            dir: cfg.pool_b.dir,
            account_count: 4,
            amount_in: 0, // use the measured intermediate delta from leg A
            min_out: 0,
        },
    }
    .pack();

    let metas = vec![
        AccountMeta::new(authority.pubkey(), true), // [0] authority (signer, payer)
        AccountMeta::new(base_ata, false),          // [1] base_ata
        AccountMeta::new(inter_ata, false),         // [2] intermediate_ata
        AccountMeta::new_readonly(cfg.leg_dex, false), // [3] leg A dex program
        AccountMeta::new(base_ata, false),          // [4] leg A user_source
        AccountMeta::new(inter_ata, false),         // [5] leg A user_dest
        AccountMeta::new(pa_in, false),             // [6] leg A pool_src
        AccountMeta::new(pa_out, false),            // [7] leg A pool_dst
        AccountMeta::new_readonly(cfg.leg_dex, false), // [8] leg B dex program
        AccountMeta::new(legb_source, false),       // [9] leg B user_source
        AccountMeta::new(base_ata, false),          // [10] leg B user_dest
        AccountMeta::new(pb_in, false),             // [11] leg B pool_src
        AccountMeta::new(pb_out, false),            // [12] leg B pool_dst
    ];

    let ix = Instruction {
        program_id: arb_id,
        accounts: metas,
        data: data.to_vec(),
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        svm.latest_blockhash(),
    );
    let res = svm.send_transaction(tx);

    let predicted_final = predicted_final(cfg);
    // Post-tx balances (on revert these are the unchanged pre-trade values — the no-movement proof).
    let base_balance = token_balance(&svm, &base_ata);
    let inter_balance = token_balance(&svm, &inter_ata);

    match res {
        Ok(meta) => {
            // post_base = base_funding - delta_in + final  =>  final = post_base + delta_in - base_funding
            let realized = base_balance as i128 + cfg.delta_in as i128 - cfg.base_funding as i128;
            Gate {
                send_ok: true,
                realized_final: u64::try_from(realized).ok(),
                predicted_final,
                cu: Some(meta.compute_units_consumed),
                err_code: None,
                base_balance,
                inter_balance,
                logs: String::new(),
            }
        }
        Err(fail) => Gate {
            send_ok: false,
            realized_final: None,
            predicted_final,
            cu: None,
            err_code: custom_code(&fail.err),
            base_balance,
            inter_balance,
            logs: format!("{:?}", fail.err),
        },
    }
}

/// Deterministic SplitMix64 PRNG — dependency-free fuzzing with reproducible counterexamples.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        lo + self.next_u64() % (hi - lo)
    }
    pub fn dir(&mut self) -> SwapDir {
        if self.next_u64() & 1 == 0 {
            SwapDir::AtoB
        } else {
            SwapDir::BtoA
        }
    }
}
