//! signer-5 — the canonical sign path: `flag → shape → caps → sign`, atomic under one mutex.
//!
//! The sidecar is the SOLE owner of the hot `SolanaSigner`; there is no public accessor for the
//! key. `sign_arb_tx` holds the caps mutex for the entire gate sequence (read signing-enabled,
//! validate shape, reserve caps, then sign) so a concurrent state change cannot interleave a sign
//! through a closed gate. Any gate failure returns the precise [`SignerError`] variant and the
//! signer is never invoked. Construction asserts the backend is `Memory` (no KMS/Fireblocks
//! round-trip on the hot path).

use std::sync::Mutex;

use solana_pubkey::Pubkey;
use solana_signature::Signature;

use super::caps::{CapReservation, PreSignCaps};
use super::error::SignerError;
use super::keychain::SolanaSigner;
use super::killswitch::KillSwitchHandle;
use super::metrics::SignerMetrics;
use super::validate::{ArbSignContext, TxShapeValidator};
use solana_program::instruction::Instruction;

/// Compiles the VALIDATED instruction list + a recent blockhash into the exact v0 message bytes
/// that get signed. Binding the signed bytes to the *same* `instructions` the validator inspected
/// closes the validate-one / sign-another gap: the sidecar never signs an artifact the shape gate
/// did not approve. The real implementation uses `solana-message` (the v0-assembly seam, deferred
/// for M1); tests inject a deterministic mock.
pub trait MessageCompiler {
    fn compile(
        &self,
        instructions: &[Instruction],
        recent_blockhash: &[u8; 32],
    ) -> Result<Vec<u8>, SignerError>;
}

/// The in-process signing sidecar — the only outflow gate of the bot.
pub struct SignerSidecar {
    signer: Box<dyn SolanaSigner>,
    hot_pubkey: Pubkey,
    flag: KillSwitchHandle,
    caps: Mutex<PreSignCaps>,
    validator: TxShapeValidator,
    metrics: SignerMetrics,
}

impl SignerSidecar {
    /// Build a sidecar. Rejects a non-Memory backend on the hot path (returns `Err`).
    pub fn new(
        signer: Box<dyn SolanaSigner>,
        flag: KillSwitchHandle,
        caps: PreSignCaps,
        validator: TxShapeValidator,
    ) -> Result<Self, SignerError> {
        if !signer.backend_kind().is_hot_path_permitted() {
            return Err(SignerError::NonMemoryBackendOnHotPath);
        }
        let hot_pubkey = signer.pubkey();
        Ok(Self {
            signer,
            hot_pubkey,
            flag,
            caps: Mutex::new(caps),
            validator,
            metrics: SignerMetrics::new(),
        })
    }

    /// The hot pubkey (public is fine — only the secret is hidden).
    pub fn hot_pubkey(&self) -> Pubkey {
        self.hot_pubkey
    }

    pub fn metrics(&self) -> &SignerMetrics {
        &self.metrics
    }

    /// A cloneable handle to the kill-switch flag (for the supervisor/admin).
    pub fn kill_switch(&self) -> KillSwitchHandle {
        self.flag.clone()
    }

    /// THE single hot-path entry that touches the hot key. The signed bytes are compiled by
    /// `compiler` from the SAME `instructions` the validator inspects (bound to `recent_blockhash`),
    /// so the signature is provably over the validated shape — there is no opaque `message_bytes`
    /// the gate never saw.
    pub fn sign_arb_tx(
        &self,
        instructions: &[Instruction],
        ctx: &ArbSignContext,
        compiler: &dyn MessageCompiler,
        recent_blockhash: &[u8; 32],
        now_millis: u64,
    ) -> Result<Signature, SignerError> {
        // Hold the caps mutex across the WHOLE gate sequence so no state change interleaves.
        let mut caps = self.caps.lock().unwrap();

        if !self.flag.signing_enabled() {
            self.metrics.inc_halt_blocked();
            return Err(SignerError::Halted);
        }

        let shape = self.validator.validate(instructions, ctx).map_err(|r| {
            self.metrics.inc_shape_rejection();
            SignerError::ShapeRejected(r)
        })?;

        let reservation: CapReservation = caps
            .reserve(shape.observed_lamport_out, now_millis)
            .map_err(|c| {
                self.metrics.inc_cap_exceeded();
                SignerError::CapExceeded(c)
            })?;

        // Compile the message from the validated instructions — the signed bytes cannot diverge
        // from the approved shape.
        let message_bytes = match compiler.compile(instructions, recent_blockhash) {
            Ok(b) => b,
            Err(e) => {
                caps.release(reservation, now_millis);
                self.metrics.inc_backend_error();
                return Err(e);
            }
        };

        match self.signer.try_sign_message(&message_bytes) {
            Ok(sig) => {
                self.metrics.inc_signature();
                Ok(sig)
            }
            Err(e) => {
                // Roll back the reservation so a backend failure does not leak budget.
                caps.release(reservation, now_millis);
                self.metrics.inc_backend_error();
                Err(e)
            }
        }
    }

    /// Reserve/release passthroughs for the landing rebuild loop (dec-2 lifecycle): the loop carries
    /// one reservation across rebuilds of the same opportunity.
    pub fn release_reservation(&self, res: CapReservation, now_millis: u64) {
        self.caps.lock().unwrap().release(res, now_millis);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::caps::PreSignCaps;
    use crate::signer::keychain::BackendKind;
    use crate::signer::validate::ShapeReject;
    use crate::txbuilder::compute::ComputeBudgetParams;
    use crate::txbuilder::wsol::{derive_ata, wrap_native};
    use arb_config::program_ids::{NATIVE_MINT, RAYDIUM_CPMM, SYSTEM_PROGRAM, TOKEN_PROGRAM};
    use solana_keypair::Keypair;
    use solana_program::instruction::{AccountMeta, Instruction};
    use solana_signer::Signer;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// A signer that counts how many times the key was touched (proves gates never reach it).
    struct CountingSigner {
        inner: Keypair,
        pubkey: Pubkey,
        calls: Arc<AtomicU64>,
    }

    impl SolanaSigner for CountingSigner {
        fn pubkey(&self) -> Pubkey {
            self.pubkey
        }
        fn try_sign_message(&self, msg: &[u8]) -> Result<Signature, SignerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.inner.sign_message(msg))
        }
        fn backend_kind(&self) -> BackendKind {
            BackendKind::Memory
        }
    }

    /// Deterministic stand-in for the real v0-message compiler: serializes blockhash + each
    /// instruction's program id and data, so the signed bytes are a pure function of the validated
    /// instruction list.
    struct MockCompiler;
    impl MessageCompiler for MockCompiler {
        fn compile(
            &self,
            instructions: &[Instruction],
            recent_blockhash: &[u8; 32],
        ) -> Result<Vec<u8>, SignerError> {
            let mut out = recent_blockhash.to_vec();
            for ix in instructions {
                out.extend_from_slice(ix.program_id.as_ref());
                out.extend_from_slice(&ix.data);
            }
            Ok(out)
        }
    }

    const BH: [u8; 32] = [9u8; 32];

    fn build(
        authority: Pubkey,
        arb_program: Pubkey,
    ) -> (TxShapeValidator, ArbSignContext, Vec<Instruction>) {
        let validator = TxShapeValidator {
            arb_program_id: arb_program,
            authority,
            max_lamport_out: 1_000_000,
            max_tip_lamports: 500_000,
            tip_accounts: vec![Pubkey::new_from_array([200; 32])],
        };
        let ctx = ArbSignContext {
            expected_lamport_out: 0,
            tip_lamports: 0,
            tip_dest: None,
            swap_programs: vec![RAYDIUM_CPMM],
            alt_addresses: vec![],
            signers: vec![authority],
            base_mint: NATIVE_MINT,
            base_ata: derive_ata(&authority, &NATIVE_MINT, &TOKEN_PROGRAM),
        };
        let mut ixs = ComputeBudgetParams::from_measured(180_000, 50).instructions();
        let wsol = wrap_native(&authority, 1_000_000, true);
        ixs.extend(wsol.pre);
        ixs.push(Instruction {
            program_id: arb_program,
            accounts: vec![AccountMeta::new(authority, true)],
            data: vec![0u8; 8],
        });
        ixs.extend(wsol.post);
        (validator, ctx, ixs)
    }

    fn sidecar_with(calls: Arc<AtomicU64>, validator: TxShapeValidator) -> (SignerSidecar, Pubkey) {
        let kp = Keypair::new();
        let pk = kp.pubkey();
        let signer = CountingSigner {
            inner: kp,
            pubkey: pk,
            calls,
        };
        let caps = PreSignCaps::new(60_000, 5, 1_000_000);
        let sc =
            SignerSidecar::new(Box::new(signer), KillSwitchHandle::new(), caps, validator).unwrap();
        (sc, pk)
    }

    #[test]
    fn happy_path_signs_and_increments_metric() {
        let authority = Pubkey::new_from_array([1; 32]);
        let arb_program = Pubkey::new_from_array([123; 32]);
        let (validator, ctx, ixs) = build(authority, arb_program);
        let calls = Arc::new(AtomicU64::new(0));
        let (sc, pk) = sidecar_with(calls.clone(), validator);
        let sig = sc.sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0).unwrap();
        // The signature is over the bytes compiled from the SAME validated instructions.
        let expected = MockCompiler.compile(&ixs, &BH).unwrap();
        assert!(sig.verify(pk.as_ref(), &expected));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sc.metrics().signatures(), 1);
    }

    #[test]
    fn halt_blocks_sign_without_touching_key() {
        let authority = Pubkey::new_from_array([1; 32]);
        let arb_program = Pubkey::new_from_array([123; 32]);
        let (validator, ctx, ixs) = build(authority, arb_program);
        let calls = Arc::new(AtomicU64::new(0));
        let (sc, _pk) = sidecar_with(calls.clone(), validator);
        sc.kill_switch()
            .halt(crate::signer::killswitch::HaltReason::Manual {
                operator: "op".into(),
            });
        assert_eq!(
            sc.sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0),
            Err(SignerError::Halted)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0); // key never touched
        assert_eq!(sc.metrics().halt_blocked(), 1);
    }

    #[test]
    fn shape_rejection_does_not_touch_key() {
        let authority = Pubkey::new_from_array([1; 32]);
        let arb_program = Pubkey::new_from_array([123; 32]);
        let (validator, mut ctx, ixs) = build(authority, arb_program);
        ctx.swap_programs = vec![Pubkey::new_from_array([9; 32])]; // not allowlisted
        let calls = Arc::new(AtomicU64::new(0));
        let (sc, _pk) = sidecar_with(calls.clone(), validator);
        let err = sc
            .sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0)
            .unwrap_err();
        assert!(matches!(
            err,
            SignerError::ShapeRejected(ShapeReject::UnauthorizedSwapProgram(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(sc.metrics().shape_rejections(), 1);
    }

    #[test]
    fn cap_exhaustion_blocks_further_signs() {
        let authority = Pubkey::new_from_array([1; 32]);
        let arb_program = Pubkey::new_from_array([123; 32]);
        let (validator, ctx, ixs) = build(authority, arb_program);
        let calls = Arc::new(AtomicU64::new(0));
        // caps allow only 2 sigs/window.
        let kp = Keypair::new();
        let pk = kp.pubkey();
        let signer = CountingSigner {
            inner: kp,
            pubkey: pk,
            calls: calls.clone(),
        };
        let caps = PreSignCaps::new(60_000, 2, 1_000_000);
        let sc =
            SignerSidecar::new(Box::new(signer), KillSwitchHandle::new(), caps, validator).unwrap();
        sc.sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0).unwrap();
        sc.sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0).unwrap();
        let err = sc
            .sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0)
            .unwrap_err();
        assert!(matches!(err, SignerError::CapExceeded(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2); // only the two within cap touched the key
        assert_eq!(sc.metrics().cap_exceeded(), 1);
    }

    #[test]
    fn rejects_non_memory_backend_at_construction() {
        struct KmsStub(Pubkey);
        impl SolanaSigner for KmsStub {
            fn pubkey(&self) -> Pubkey {
                self.0
            }
            fn try_sign_message(&self, _m: &[u8]) -> Result<Signature, SignerError> {
                unreachable!()
            }
            fn backend_kind(&self) -> BackendKind {
                BackendKind::Kms
            }
        }
        let validator = TxShapeValidator {
            arb_program_id: Pubkey::new_from_array([123; 32]),
            authority: Pubkey::new_from_array([1; 32]),
            max_lamport_out: 0,
            max_tip_lamports: 0,
            tip_accounts: vec![],
        };
        let r = SignerSidecar::new(
            Box::new(KmsStub(Pubkey::new_from_array([5; 32]))),
            KillSwitchHandle::new(),
            PreSignCaps::new(1, 1, 1),
            validator,
        );
        assert!(matches!(r, Err(SignerError::NonMemoryBackendOnHotPath)));
    }

    #[test]
    fn foreign_destination_in_system_transfer_is_rejected() {
        let authority = Pubkey::new_from_array([1; 32]);
        let arb_program = Pubkey::new_from_array([123; 32]);
        let (validator, ctx, mut ixs) = build(authority, arb_program);
        // Append a transfer to an attacker address.
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&999u64.to_le_bytes());
        ixs.push(Instruction {
            program_id: SYSTEM_PROGRAM,
            accounts: vec![
                AccountMeta::new(authority, true),
                AccountMeta::new(Pubkey::new_from_array([88; 32]), false),
            ],
            data,
        });
        let calls = Arc::new(AtomicU64::new(0));
        let (sc, _pk) = sidecar_with(calls.clone(), validator);
        assert!(matches!(
            sc.sign_arb_tx(&ixs, &ctx, &MockCompiler, &BH, 0),
            Err(SignerError::ShapeRejected(ShapeReject::ForeignDestination(
                _
            )))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
