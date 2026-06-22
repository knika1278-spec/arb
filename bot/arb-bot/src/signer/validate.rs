//! signer-3 — `TxShapeValidator`: independently re-derive the arb-tx shape from the instruction
//! list and HARD-REJECT anything off-template, BEFORE the key is touched.
//!
//! The on-chain `TryArbitrage` program is the *authoritative* trust boundary (invariant §6: it
//! verifies every swap-CPI target is allowlisted and every balance-read account is bot-owned). This
//! validator is the off-chain mirror / defense-in-depth: it parses the top-level instructions the
//! executor assembled and checks, from the bytes:
//!
//! - every top-level program id is in the infra+arb allowlist (ComputeBudget/System/Token/Token-2022/
//!   ATA + the arb program), and the declared swap programs are Wave-1 DEX-allowlisted;
//! - every System-transfer destination is either an owned account (the WSOL ATA / authority) or a
//!   resolved Jito tip account — anything else is a `ForeignDestination`;
//! - total lamport-out (transfers that actually leave: the tip) is within `max_lamport_out` and is
//!   not *under*-declared by the caller (`ExpectedOutMismatch`);
//! - the tip goes to a tip account, within the tip cap;
//! - no required signer was placed in the ALT (signers can never be ALT-resolved);
//! - **add-2:** the route closes back to the bot-owned base ATA (round-trip closure).
//!
//! Operating on the pre-compile instruction list (rather than a compiled `VersionedMessage`) is the
//! host-green seam: the executor compiles to v0 with `solana-message` at the landing edge; the keys
//! the validator inspects are already concrete (no ALT *index* resolution is needed pre-compile),
//! and the ALT membership is supplied via [`ArbSignContext::alt_addresses`].

use arb_config::program_ids::{
    is_allowlisted_swap_program, ASSOCIATED_TOKEN_PROGRAM, COMPUTE_BUDGET_PROGRAM, NATIVE_MINT,
    SYSTEM_PROGRAM, TOKEN_2022_PROGRAM, TOKEN_PROGRAM,
};
use solana_program::instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::txbuilder::wsol::derive_ata;

/// System-program `Transfer` enum tag (little-endian u32) — matches `txbuilder::wsol`. It is the
/// ONLY System opcode the arb template emits; every other System opcode (CreateAccount,
/// CreateAccountWithSeed, TransferWithSeed, WithdrawNonceAccount, …) can move lamports to a foreign
/// destination and is HARD-REJECTED.
const SYSTEM_TRANSFER_TAG: u32 = 2;

// SPL Token / Token-2022 opcodes (data[0]). The arb template only ever emits SyncNative + a
// CloseAccount that unwraps to an OWNED destination, at the top level — the swap CPIs live INSIDE
// the arb instruction, not as siblings. Every other opcode (Transfer/TransferChecked/Approve/
// SetAuthority/Burn/MintTo/…) is HARD-REJECTED so the hot key cannot sign an SPL drain.
const TOKEN_CLOSE_ACCOUNT: u8 = 9;
const TOKEN_SYNC_NATIVE: u8 = 17;

// Associated-token-account opcodes: legacy `Create` (empty data) and `CreateIdempotent` (data[0]=1)
// both fund the OWNED ATA from the payer; anything else is rejected.
const ATA_CREATE: u8 = 0;
const ATA_CREATE_IDEMPOTENT: u8 = 1;

/// Side-channel the caller passes so the validator can mirror the route (signers are never in the
/// ALT — asserted) and charge caps with the right lamport-out.
#[derive(Clone, Debug, Default)]
pub struct ArbSignContext {
    /// Caller's declared total lamport outflow (the tip; fees are charged by the runtime).
    pub expected_lamport_out: u64,
    /// Declared Jito tip (0 in Fase 1).
    pub tip_lamports: u64,
    /// The resolved tip account the tip transfer targets (Fase 2).
    pub tip_dest: Option<Pubkey>,
    /// Swap programs the route invokes (mirrored against the DEX allowlist).
    pub swap_programs: Vec<Pubkey>,
    /// Union of all addresses held in the route's ALTs (for the signer-in-ALT check).
    pub alt_addresses: Vec<Pubkey>,
    /// Required signer pubkeys (authority).
    pub signers: Vec<Pubkey>,
    /// The base mint the round-trip must return to (add-2 closure).
    pub base_mint: Pubkey,
    /// The bot-owned base ATA the round-trip must close to (add-2 closure).
    pub base_ata: Pubkey,
}

/// A HARD reject — no signature is produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeReject {
    ProgramNotAllowlisted(Pubkey),
    UnauthorizedSwapProgram(Pubkey),
    ForeignDestination(Pubkey),
    LamportOutOverCap {
        requested: u64,
        cap: u64,
    },
    ExpectedOutMismatch {
        observed: u64,
        declared: u64,
    },
    TipNotTipAccount(Pubkey),
    TipOverCap {
        requested: u64,
        cap: u64,
    },
    TipMismatch {
        observed: u64,
        declared: u64,
    },
    SignerInAlt(Pubkey),
    MissingArbInstruction,
    /// add-2: the declared base ATA is not the bot-owned ATA for the base mint.
    RouteDoesNotCloseToBaseAta {
        claimed: Pubkey,
        expected: Pubkey,
    },
    /// A System opcode other than `Transfer` (could move lamports off-template).
    DisallowedSystemOpcode {
        tag: u32,
    },
    /// An SPL Token / Token-2022 opcode other than SyncNative / CloseAccount-to-owned (e.g. a
    /// top-level `Transfer`/`Approve`/`SetAuthority` that would drain an owned token account).
    DisallowedTokenOpcode {
        tag: u8,
    },
    /// An Associated-Token-Account opcode other than Create / CreateIdempotent.
    DisallowedAtaOpcode {
        tag: u8,
    },
    /// An instruction was too short to parse its opcode/accounts.
    MalformedInstruction,
}

/// What a passing validation yields (observed outflow the caller charges against caps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedShape {
    pub observed_lamport_out: u64,
    pub observed_tip: u64,
}

/// Static validator config: the deployed arb program, the bot authority, the outflow caps, and the
/// runtime-resolved Jito tip accounts.
#[derive(Clone, Debug)]
pub struct TxShapeValidator {
    pub arb_program_id: Pubkey,
    pub authority: Pubkey,
    pub max_lamport_out: u64,
    pub max_tip_lamports: u64,
    pub tip_accounts: Vec<Pubkey>,
}

/// Per-instruction effect the walk accumulates.
#[derive(Clone, Copy, Default)]
struct IxEffect {
    /// Lamports that LEAVE the bot (a tip transfer to a Jito tip account).
    out: u64,
    /// Lamports that go to a Jito tip account (subset of `out`).
    tip: u64,
    /// Whether this is the arb program's instruction.
    is_arb: bool,
}

impl TxShapeValidator {
    /// True if `dest` is a bot-owned account (the WSOL ATA, the authority, or the base ATA).
    fn is_owned(&self, dest: &Pubkey, own_wsol_ata: &Pubkey, ctx: &ArbSignContext) -> bool {
        *dest == *own_wsol_ata || *dest == self.authority || *dest == ctx.base_ata
    }

    /// Payload-inspect ONE top-level instruction and return its on-template effect, or HARD-REJECT.
    fn classify_instruction(
        &self,
        ix: &Instruction,
        own_wsol_ata: &Pubkey,
        ctx: &ArbSignContext,
    ) -> Result<IxEffect, ShapeReject> {
        let p = ix.program_id;

        if p == COMPUTE_BUDGET_PROGRAM {
            // ComputeBudget moves no funds and takes no accounts.
            return Ok(IxEffect::default());
        }
        if p == self.arb_program_id {
            return Ok(IxEffect {
                is_arb: true,
                ..IxEffect::default()
            });
        }
        if p == SYSTEM_PROGRAM {
            // ONLY Transfer (tag 2) is on-template; reject every other (lamport-moving) opcode.
            let tag = ix
                .data
                .get(0..4)
                .and_then(|b| b.try_into().ok())
                .map(u32::from_le_bytes);
            return match tag {
                Some(SYSTEM_TRANSFER_TAG) => {
                    if ix.data.len() < 12 || ix.accounts.len() < 2 {
                        return Err(ShapeReject::MalformedInstruction);
                    }
                    let lamports = u64::from_le_bytes(ix.data[4..12].try_into().unwrap());
                    let dest = ix.accounts[1].pubkey;
                    if self.is_owned(&dest, own_wsol_ata, ctx) {
                        Ok(IxEffect::default()) // wrapping own SOL into the owned ATA — not outflow
                    } else if self.tip_accounts.contains(&dest) {
                        Ok(IxEffect {
                            out: lamports,
                            tip: lamports,
                            ..IxEffect::default()
                        })
                    } else {
                        Err(ShapeReject::ForeignDestination(dest))
                    }
                }
                Some(other) => Err(ShapeReject::DisallowedSystemOpcode { tag: other }),
                None => Err(ShapeReject::MalformedInstruction),
            };
        }
        if p == TOKEN_PROGRAM || p == TOKEN_2022_PROGRAM {
            // The swap CPIs live INSIDE the arb instruction; the only top-level Token instructions
            // the template emits are SyncNative (no funds leave) and CloseAccount (unwrap to OWNED).
            return match ix.data.first().copied() {
                Some(TOKEN_SYNC_NATIVE) => Ok(IxEffect::default()),
                Some(TOKEN_CLOSE_ACCOUNT) => {
                    // CloseAccount accounts = [account, destination, owner]; destination MUST be owned.
                    match ix.accounts.get(1).map(|a| a.pubkey) {
                        Some(d) if self.is_owned(&d, own_wsol_ata, ctx) => Ok(IxEffect::default()),
                        Some(d) => Err(ShapeReject::ForeignDestination(d)),
                        None => Err(ShapeReject::MalformedInstruction),
                    }
                }
                Some(t) => Err(ShapeReject::DisallowedTokenOpcode { tag: t }),
                None => Err(ShapeReject::MalformedInstruction),
            };
        }
        if p == ASSOCIATED_TOKEN_PROGRAM {
            // Create / CreateIdempotent fund the OWNED ATA from the payer; empty data = legacy Create.
            return match ix.data.first().copied() {
                None | Some(ATA_CREATE) | Some(ATA_CREATE_IDEMPOTENT) => Ok(IxEffect::default()),
                Some(t) => Err(ShapeReject::DisallowedAtaOpcode { tag: t }),
            };
        }
        Err(ShapeReject::ProgramNotAllowlisted(p))
    }

    /// Validate the assembled instruction list against the arb template.
    pub fn validate(
        &self,
        instructions: &[Instruction],
        ctx: &ArbSignContext,
    ) -> Result<ValidatedShape, ShapeReject> {
        // 1. Signers must never be ALT-resolvable.
        for s in &ctx.signers {
            if ctx.alt_addresses.contains(s) {
                return Err(ShapeReject::SignerInAlt(*s));
            }
        }

        // 2. Declared swap programs mirror the on-chain DEX allowlist.
        for p in &ctx.swap_programs {
            if !is_allowlisted_swap_program(p) {
                return Err(ShapeReject::UnauthorizedSwapProgram(*p));
            }
        }

        // 3. add-2 — round-trip closure: the base ATA must be the bot-owned ATA for the base mint.
        // The base mint is a classic-SPL or Token-2022 mint; the WSOL base resolves under the
        // classic Token program. We accept whichever token program derives the claimed ATA.
        let expected_classic = derive_ata(&self.authority, &ctx.base_mint, &TOKEN_PROGRAM);
        let expected_t22 = derive_ata(&self.authority, &ctx.base_mint, &TOKEN_2022_PROGRAM);
        if ctx.base_ata != expected_classic && ctx.base_ata != expected_t22 {
            return Err(ShapeReject::RouteDoesNotCloseToBaseAta {
                claimed: ctx.base_ata,
                expected: expected_classic,
            });
        }

        // 4. Walk instructions: STRICT per-program opcode whitelist + destination classification.
        // Every top-level instruction is payload-inspected — a permissive program-id allowlist is
        // not enough, because a top-level SPL-Token Transfer or a non-Transfer System opcode could
        // move funds to a foreign destination outside the on-chain arb program's reach.
        let own_wsol_ata = derive_ata(&self.authority, &NATIVE_MINT, &TOKEN_PROGRAM);
        let mut observed_out = 0u64;
        let mut observed_tip = 0u64;
        let mut has_arb = false;
        for ix in instructions {
            let eff = self.classify_instruction(ix, &own_wsol_ata, ctx)?;
            observed_out = observed_out.saturating_add(eff.out);
            observed_tip = observed_tip.saturating_add(eff.tip);
            has_arb |= eff.is_arb;
        }
        if !has_arb {
            return Err(ShapeReject::MissingArbInstruction);
        }

        // 5. Tip checks (Fase 2 — skipped when no tip declared).
        if ctx.tip_lamports > 0 {
            if let Some(dest) = ctx.tip_dest {
                if !self.tip_accounts.contains(&dest) {
                    return Err(ShapeReject::TipNotTipAccount(dest));
                }
            }
            if observed_tip != ctx.tip_lamports {
                return Err(ShapeReject::TipMismatch {
                    observed: observed_tip,
                    declared: ctx.tip_lamports,
                });
            }
            if observed_tip > self.max_tip_lamports {
                return Err(ShapeReject::TipOverCap {
                    requested: observed_tip,
                    cap: self.max_tip_lamports,
                });
            }
        }

        // 6. Outflow caps.
        if observed_out > self.max_lamport_out {
            return Err(ShapeReject::LamportOutOverCap {
                requested: observed_out,
                cap: self.max_lamport_out,
            });
        }
        // Caller must declare at least the observed outflow (under-declaration is a bug/attack).
        if observed_out > ctx.expected_lamport_out {
            return Err(ShapeReject::ExpectedOutMismatch {
                observed: observed_out,
                declared: ctx.expected_lamport_out,
            });
        }

        Ok(ValidatedShape {
            observed_lamport_out: observed_out,
            observed_tip,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txbuilder::compute::ComputeBudgetParams;
    use crate::txbuilder::wsol::wrap_native;
    use solana_program::instruction::{AccountMeta, Instruction};

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn validator() -> TxShapeValidator {
        TxShapeValidator {
            arb_program_id: key(123),
            authority: key(1),
            max_lamport_out: 1_000_000,
            max_tip_lamports: 500_000,
            tip_accounts: vec![key(200), key(201)],
        }
    }

    fn ctx_for(authority: Pubkey) -> ArbSignContext {
        ArbSignContext {
            expected_lamport_out: 0,
            tip_lamports: 0,
            tip_dest: None,
            swap_programs: vec![arb_config::program_ids::RAYDIUM_CPMM],
            alt_addresses: vec![],
            signers: vec![authority],
            base_mint: NATIVE_MINT,
            base_ata: derive_ata(&authority, &NATIVE_MINT, &TOKEN_PROGRAM),
        }
    }

    fn arb_ix(program: Pubkey, authority: Pubkey) -> Instruction {
        Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(authority, true)],
            data: vec![0u8; 8],
        }
    }

    /// ComputeBudget(2) + WSOL wrap(3) + arb(1) + WSOL close(1) — the canonical Fase-1 template.
    fn canonical_tx(
        authority: Pubkey,
        arb_program: Pubkey,
        wrap_lamports: u64,
    ) -> Vec<Instruction> {
        let mut ixs = ComputeBudgetParams::from_measured(180_000, 50).instructions();
        let wsol = wrap_native(&authority, wrap_lamports, true);
        ixs.extend(wsol.pre);
        ixs.push(arb_ix(arb_program, authority));
        ixs.extend(wsol.post);
        ixs
    }

    #[test]
    fn accepts_canonical_two_leg_arb_template() {
        let v = validator();
        let ixs = canonical_tx(v.authority, v.arb_program_id, 1_000_000);
        let shape = v
            .validate(&ixs, &ctx_for(v.authority))
            .expect("template valid");
        // Wrapping own SOL into the owned WSOL ATA is not outflow.
        assert_eq!(shape.observed_lamport_out, 0);
    }

    #[test]
    fn rejects_cpi_to_non_allowlisted_program() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // Inject an instruction to a random program.
        ixs.push(arb_ix(key(250), v.authority));
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::ProgramNotAllowlisted(key(250)))
        );
    }

    #[test]
    fn rejects_non_allowlisted_swap_program() {
        let v = validator();
        let ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        let mut ctx = ctx_for(v.authority);
        ctx.swap_programs = vec![key(99)];
        assert_eq!(
            v.validate(&ixs, &ctx),
            Err(ShapeReject::UnauthorizedSwapProgram(key(99)))
        );
    }

    #[test]
    fn rejects_transfer_to_foreign_destination() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // A system transfer to an attacker address.
        let foreign = key(77);
        let mut data = Vec::new();
        data.extend_from_slice(&SYSTEM_TRANSFER_TAG.to_le_bytes());
        data.extend_from_slice(&50_000u64.to_le_bytes());
        ixs.push(Instruction {
            program_id: SYSTEM_PROGRAM,
            accounts: vec![
                AccountMeta::new(v.authority, true),
                AccountMeta::new(foreign, false),
            ],
            data,
        });
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::ForeignDestination(foreign))
        );
    }

    #[test]
    fn rejects_signer_in_alt() {
        let v = validator();
        let ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        let mut ctx = ctx_for(v.authority);
        ctx.alt_addresses = vec![v.authority]; // signer smuggled into the ALT
        assert_eq!(
            v.validate(&ixs, &ctx),
            Err(ShapeReject::SignerInAlt(v.authority))
        );
    }

    #[test]
    fn expected_out_mismatch_on_under_declaration() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // A legit tip transfer of 100_000 to a tip account, but caller declares 0 out.
        let mut data = Vec::new();
        data.extend_from_slice(&SYSTEM_TRANSFER_TAG.to_le_bytes());
        data.extend_from_slice(&100_000u64.to_le_bytes());
        ixs.push(Instruction {
            program_id: SYSTEM_PROGRAM,
            accounts: vec![
                AccountMeta::new(v.authority, true),
                AccountMeta::new(key(200), false), // a tip account
            ],
            data,
        });
        let mut ctx = ctx_for(v.authority);
        ctx.expected_lamport_out = 0; // under-declares the 100k tip outflow
        assert_eq!(
            v.validate(&ixs, &ctx),
            Err(ShapeReject::ExpectedOutMismatch {
                observed: 100_000,
                declared: 0
            })
        );
    }

    #[test]
    fn tip_to_non_tip_account_is_rejected() {
        let v = validator();
        let ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        let mut ctx = ctx_for(v.authority);
        ctx.tip_lamports = 10_000;
        ctx.tip_dest = Some(key(55)); // not a tip account
        ctx.expected_lamport_out = 10_000;
        assert_eq!(
            v.validate(&ixs, &ctx),
            Err(ShapeReject::TipNotTipAccount(key(55)))
        );
    }

    #[test]
    fn rejects_route_not_closing_to_base_ata() {
        let v = validator();
        let ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        let mut ctx = ctx_for(v.authority);
        ctx.base_ata = key(66); // not the bot-owned base ATA
        assert!(matches!(
            v.validate(&ixs, &ctx),
            Err(ShapeReject::RouteDoesNotCloseToBaseAta { .. })
        ));
    }

    #[test]
    fn missing_arb_instruction_is_rejected() {
        let v = validator();
        // ComputeBudget only — no arb instruction.
        let ixs = ComputeBudgetParams::from_measured(100_000, 0).instructions();
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::MissingArbInstruction)
        );
    }

    // ---- critical: top-level SPL-Token / System opcode drains must be HARD-REJECTED ----

    #[test]
    fn rejects_top_level_spl_token_transfer_drain() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // Append a top-level SPL Token Transfer (tag 3) FROM the owned WSOL ATA TO an attacker.
        let wsol_ata = derive_ata(&v.authority, &NATIVE_MINT, &TOKEN_PROGRAM);
        let mut data = vec![3u8]; // Transfer opcode
        data.extend_from_slice(&u64::MAX.to_le_bytes()); // full balance
        ixs.push(Instruction {
            program_id: TOKEN_PROGRAM,
            accounts: vec![
                AccountMeta::new(wsol_ata, false),
                AccountMeta::new(key(88), false), // attacker token account
                AccountMeta::new_readonly(v.authority, true),
            ],
            data,
        });
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::DisallowedTokenOpcode { tag: 3 })
        );
    }

    #[test]
    fn rejects_token_close_to_foreign_destination() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // A CloseAccount whose rent destination is an attacker, not the authority.
        let wsol_ata = derive_ata(&v.authority, &NATIVE_MINT, &TOKEN_PROGRAM);
        ixs.push(Instruction {
            program_id: TOKEN_PROGRAM,
            accounts: vec![
                AccountMeta::new(wsol_ata, false),
                AccountMeta::new(key(88), false), // foreign destination
                AccountMeta::new_readonly(v.authority, true),
            ],
            data: vec![9u8], // CloseAccount
        });
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::ForeignDestination(key(88)))
        );
    }

    #[test]
    fn rejects_non_transfer_system_opcode() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // System CreateAccount (tag 0) funds a brand-new attacker account with lamports.
        let mut data = 0u32.to_le_bytes().to_vec(); // CreateAccount tag
        data.extend_from_slice(&900_000u64.to_le_bytes()); // lamports
        data.extend_from_slice(&0u64.to_le_bytes()); // space
        data.extend_from_slice(&[0u8; 32]); // owner
        ixs.push(Instruction {
            program_id: SYSTEM_PROGRAM,
            accounts: vec![
                AccountMeta::new(v.authority, true),
                AccountMeta::new(key(88), true),
            ],
            data,
        });
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::DisallowedSystemOpcode { tag: 0 })
        );
    }

    #[test]
    fn rejects_token_set_authority_opcode() {
        let v = validator();
        let mut ixs = canonical_tx(v.authority, v.arb_program_id, 1_000);
        // SetAuthority (tag 6) on the owned WSOL ATA would hand control to an attacker.
        let wsol_ata = derive_ata(&v.authority, &NATIVE_MINT, &TOKEN_PROGRAM);
        ixs.push(Instruction {
            program_id: TOKEN_2022_PROGRAM,
            accounts: vec![
                AccountMeta::new(wsol_ata, false),
                AccountMeta::new_readonly(v.authority, true),
            ],
            data: vec![6u8],
        });
        assert_eq!(
            v.validate(&ixs, &ctx_for(v.authority)),
            Err(ShapeReject::DisallowedTokenOpcode { tag: 6 })
        );
    }
}
