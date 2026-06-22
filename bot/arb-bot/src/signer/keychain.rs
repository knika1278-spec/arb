//! signer-2 — the `SolanaSigner` abstraction + the in-memory hot-key backend.
//!
//! The hot path may use ONLY the [`MemorySigner`] (asserted at sidecar construction); KMS / Squads
//! / Fireblocks are config-selectable seams for the *treasury* path (signer-13), never the hot
//! sign loop. The hot key lives in a `solana_keypair::Keypair` whose underlying ed25519 signing
//! key zeroizes on drop (ed25519-dalek `ZeroizeOnDrop`); the raw-bytes constructor additionally
//! wraps its input in `Zeroizing` so the seed buffer is wiped even before the `Keypair` owns it.

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use zeroize::Zeroizing;

use super::error::SignerError;

/// Which signing backend produced a signer. Only [`BackendKind::Memory`] is hot-path-legal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Memory,
    Kms,
    Squads,
    Fireblocks,
}

impl BackendKind {
    /// Whether this backend may sit on the hot sign path. Only the in-memory hot key qualifies;
    /// a KMS/Fireblocks round-trip would add network latency to the latency-critical sign.
    pub fn is_hot_path_permitted(self) -> bool {
        matches!(self, BackendKind::Memory)
    }
}

/// Solana-Keychain-style signing abstraction (the sidecar owns exactly one).
pub trait SolanaSigner: Send + Sync {
    fn pubkey(&self) -> Pubkey;
    fn try_sign_message(&self, msg: &[u8]) -> Result<Signature, SignerError>;
    fn backend_kind(&self) -> BackendKind;
}

/// In-memory hot-key backend. The ONLY backend permitted on the sign hot path.
pub struct MemorySigner {
    keypair: Keypair,
    pubkey: Pubkey,
}

impl MemorySigner {
    /// Wrap an already-loaded hot keypair (e.g. from `arb_config::secrets::load_hot_keypair`).
    pub fn new(keypair: Keypair) -> Self {
        let pubkey = keypair.pubkey();
        Self { keypair, pubkey }
    }

    /// Build from raw 64-byte keypair bytes, wiping the temporary buffer on drop.
    pub fn from_keypair_bytes(bytes: &[u8]) -> Result<Self, SignerError> {
        let z = Zeroizing::new(bytes.to_vec());
        let kp = Keypair::try_from(z.as_slice())
            .map_err(|e| SignerError::Backend(format!("invalid keypair bytes: {e}")))?;
        Ok(Self::new(kp))
    }
}

impl SolanaSigner for MemorySigner {
    fn pubkey(&self) -> Pubkey {
        self.pubkey
    }

    fn try_sign_message(&self, msg: &[u8]) -> Result<Signature, SignerError> {
        self.keypair
            .try_sign_message(msg)
            .map_err(|e| SignerError::Backend(e.to_string()))
    }

    fn backend_kind(&self) -> BackendKind {
        BackendKind::Memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_signer_produces_verifiable_signature() {
        let kp = Keypair::new();
        let pk = kp.pubkey();
        let signer = MemorySigner::new(kp);
        assert_eq!(signer.pubkey(), pk);
        assert_eq!(signer.backend_kind(), BackendKind::Memory);

        let msg = b"atomic-arb-tx-message-bytes";
        let sig = signer.try_sign_message(msg).unwrap();
        // Valid ed25519 signature verifies against the signer's pubkey.
        assert!(sig.verify(pk.as_ref(), msg));
        // A different message does not verify.
        assert!(!sig.verify(pk.as_ref(), b"tampered"));
    }

    #[test]
    fn only_memory_backend_is_hot_path_permitted() {
        assert!(BackendKind::Memory.is_hot_path_permitted());
        assert!(!BackendKind::Kms.is_hot_path_permitted());
        assert!(!BackendKind::Squads.is_hot_path_permitted());
        assert!(!BackendKind::Fireblocks.is_hot_path_permitted());
    }

    #[test]
    fn roundtrip_through_keypair_bytes() {
        let kp = Keypair::new();
        let bytes = kp.to_bytes();
        let signer = MemorySigner::from_keypair_bytes(&bytes).unwrap();
        assert_eq!(signer.pubkey(), kp.pubkey());
        // Garbage bytes are rejected, not silently accepted.
        assert!(MemorySigner::from_keypair_bytes(&[0u8; 10]).is_err());
    }
}
