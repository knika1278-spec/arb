//! The single error type returned by the signer sign path.

use super::caps::CapExceeded;
use super::validate::ShapeReject;

/// Every way `sign_arb_tx` can refuse — each variant means NO signature was produced (the gates
/// run before the key is ever touched).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerError {
    /// Kill-switch is engaged (`signing-enabled == false`).
    Halted,
    /// Tx shape did not match the arb template.
    ShapeRejected(ShapeReject),
    /// A synchronous pre-sign cap (count or cumulative lamport-out) was exceeded.
    CapExceeded(CapExceeded),
    /// The signing backend failed (e.g. malformed key bytes).
    Backend(String),
    /// A non-Memory backend was placed on the hot sign path (asserted at construction).
    NonMemoryBackendOnHotPath,
}

impl core::fmt::Display for SignerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SignerError::Halted => write!(f, "signing halted (kill-switch engaged)"),
            SignerError::ShapeRejected(r) => write!(f, "tx shape rejected: {r:?}"),
            SignerError::CapExceeded(c) => write!(f, "pre-sign cap exceeded: {c:?}"),
            SignerError::Backend(e) => write!(f, "signing backend error: {e}"),
            SignerError::NonMemoryBackendOnHotPath => {
                write!(f, "non-Memory backend rejected on hot sign path")
            }
        }
    }
}

impl std::error::Error for SignerError {}
