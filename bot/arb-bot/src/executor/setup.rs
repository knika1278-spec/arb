//! landing-1 — the Fase-0 landing **setup seam**: the runtime contracts the networked landing
//! clients (JitoClient/HeliusSender, built in landing-2/-7) plug into, with no network dependency
//! of their own so they compile and unit-test on the host today.
//!
//! Three seams, one per operator-provisioned resource:
//! - [`TipAccountSource`] — the 8 Jito tip accounts are RESOLVED AT RUNTIME via `getTipAccounts`
//!   and validated ([`TipAccountSet`] enforces exactly 8 distinct, non-default keys). They are
//!   never a hardcoded constant: Jito rotates the set, and pinning stale accounts would silently
//!   misroute tips. The real resolver (a JSON-RPC client with a TTL cache) is `landing-2`.
//! - [`JitoAuth`] — the allowlisted UUID, attached as the [`JITO_AUTH_HEADER`] on every Block
//!   Engine call. The UUID *value* is read from the env (see [`arb_config::providers::JitoConfig`]),
//!   never committed; this type only carries the resolved value + formats the header.
//! - [`SenderEndpoint`] + [`EndpointProbe`] — the Helius Sender fallback endpoint and the
//!   reachability check the operator runs at boot. The real probe (an HTTP HEAD) is network and so
//!   lands with the Sender client (`landing-7`); the seam keeps the readiness gate testable now.
//!
//! OPERATOR provisioning this task still requires (not code): obtain the Jito allowlisted UUID and
//! export it as `JITO_AUTH_UUID`, register the (free, 0-credit) Helius Sender endpoint, and confirm
//! `getTipAccounts` resolves 8 accounts against the production RPC.

use arb_config::providers::{JitoConfig, SenderConfig, JITO_AUTH_HEADER};
use solana_pubkey::Pubkey;

use super::tip::{TipOracle, TipParams};

/// Jito always exposes exactly 8 tip accounts (plan §6 / Block Engine `getTipAccounts`).
pub const TIP_ACCOUNT_COUNT: usize = 8;

/// Why a resolved tip-account set was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipAccountError {
    /// `getTipAccounts` must yield exactly [`TIP_ACCOUNT_COUNT`] accounts.
    WrongCount { got: usize },
    /// The same account appeared twice (round-robin would over-weight it).
    DuplicateAccount,
    /// The all-zero [`Pubkey::default`] is never a real tip account — a sign the set was a
    /// hardcoded placeholder rather than a runtime resolution.
    DefaultAccount,
}

impl core::fmt::Display for TipAccountError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TipAccountError::WrongCount { got } => {
                write!(f, "expected {TIP_ACCOUNT_COUNT} tip accounts, got {got}")
            }
            TipAccountError::DuplicateAccount => write!(f, "duplicate tip account"),
            TipAccountError::DefaultAccount => write!(f, "default (all-zero) tip account"),
        }
    }
}

impl std::error::Error for TipAccountError {}

/// A validated set of exactly 8 distinct Jito tip accounts resolved at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipAccountSet([Pubkey; TIP_ACCOUNT_COUNT]);

impl TipAccountSet {
    /// Validate a freshly-resolved set (e.g. the parsed `getTipAccounts` response). Rejects the
    /// wrong count, any duplicate, and the all-zero placeholder — so a hardcoded/empty set can
    /// never reach the tip oracle.
    pub fn from_resolved(accounts: Vec<Pubkey>) -> Result<Self, TipAccountError> {
        if accounts.len() != TIP_ACCOUNT_COUNT {
            return Err(TipAccountError::WrongCount {
                got: accounts.len(),
            });
        }
        for (i, a) in accounts.iter().enumerate() {
            if *a == Pubkey::default() {
                return Err(TipAccountError::DefaultAccount);
            }
            if accounts[..i].contains(a) {
                return Err(TipAccountError::DuplicateAccount);
            }
        }
        let mut arr = [Pubkey::default(); TIP_ACCOUNT_COUNT];
        arr.copy_from_slice(&accounts);
        Ok(Self(arr))
    }

    pub fn as_slice(&self) -> &[Pubkey] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<Pubkey> {
        self.0.to_vec()
    }
}

/// Runtime source of the 8 Jito tip accounts. The production impl (`landing-2`) calls
/// `getTipAccounts` over JSON-RPC with a TTL cache; it MUST resolve at runtime and MUST NOT return
/// a compiled-in constant. Kept as a trait so the executor builds its tip oracle from whatever
/// source is wired without depending on the networked client.
pub trait TipAccountSource {
    /// Resolve and validate the current tip-account set.
    fn resolve(&self) -> Result<TipAccountSet, TipAccountError>;

    /// Convenience: resolve the set and build a ready [`TipOracle`] over it.
    fn resolve_oracle(&self, params: TipParams) -> Result<TipOracle, TipAccountError> {
        Ok(TipOracle::from_account_set(params, &self.resolve()?))
    }
}

/// Why a Jito auth UUID was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// The resolved UUID was blank.
    Empty,
    /// The UUID was implausibly short (a UUIDv4 is 36 chars) — likely a misconfigured env var.
    TooShort,
}

impl core::fmt::Display for AuthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AuthError::Empty => write!(f, "jito auth uuid is empty"),
            AuthError::TooShort => write!(f, "jito auth uuid is implausibly short"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The allowlisted Jito UUID, ready to attach as the `x-jito-auth` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JitoAuth(String);

impl JitoAuth {
    /// Smallest plausible length for an allowlisted UUID (guards an obvious misconfig without
    /// hard-coupling to the exact UUIDv4 shape, which Jito could change).
    const MIN_UUID_LEN: usize = 8;

    /// Wrap a resolved UUID, rejecting a blank or implausibly short value.
    pub fn new(uuid: impl Into<String>) -> Result<Self, AuthError> {
        let uuid = uuid.into().trim().to_string();
        if uuid.is_empty() {
            return Err(AuthError::Empty);
        }
        if uuid.len() < Self::MIN_UUID_LEN {
            return Err(AuthError::TooShort);
        }
        Ok(Self(uuid))
    }

    /// Resolve from config: `Ok(None)` when the operator has not provisioned a UUID yet (the
    /// client then runs on the public rate-limit), `Ok(Some)` when present + valid, `Err` when a
    /// provisioned value is malformed.
    pub fn from_jito_config(cfg: &JitoConfig) -> Result<Option<Self>, AuthError> {
        match cfg.resolve_auth_uuid() {
            Some(u) => Ok(Some(Self::new(u)?)),
            None => Ok(None),
        }
    }

    /// The `(x-jito-auth, <uuid>)` header pair for the Block Engine client.
    pub fn header(&self) -> (&'static str, &str) {
        (JITO_AUTH_HEADER, &self.0)
    }

    pub fn uuid(&self) -> &str {
        &self.0
    }
}

/// The Helius Sender fallback endpoint (non-secret URL + routing flags), mirrored from
/// [`SenderConfig`]. The reachability check is the [`EndpointProbe`] seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderEndpoint {
    pub url: String,
    /// `true` => SWQoS-only (cheap, 0.000005 SOL min tip); `false` => dual SWQoS+Jito.
    pub swqos_only: bool,
    /// Whether the operator has enabled the fallback (routing-exclusivity still applies at send).
    pub enabled: bool,
}

impl SenderEndpoint {
    pub fn from_config(cfg: &SenderConfig) -> Self {
        Self {
            url: cfg.url.clone(),
            swqos_only: cfg.swqos_only,
            enabled: cfg.enabled,
        }
    }
}

/// Boot-time reachability check for a landing endpoint. The real impl issues an HTTP HEAD/health
/// request (network — lands with the Sender client in `landing-7`); the seam lets the boot
/// readiness gate be exercised without the network.
pub trait EndpointProbe {
    /// Whether the endpoint answered a health probe.
    fn reachable(&self, endpoint: &SenderEndpoint) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> Vec<Pubkey> {
        (0..n as u8)
            .map(|i| Pubkey::new_from_array([i + 1; 32])) // +1 so none is the all-zero default
            .collect()
    }

    #[test]
    fn tip_set_accepts_exactly_eight_distinct() {
        let set = TipAccountSet::from_resolved(keys(8)).unwrap();
        assert_eq!(set.as_slice().len(), TIP_ACCOUNT_COUNT);
    }

    #[test]
    fn tip_set_rejects_wrong_count() {
        assert_eq!(
            TipAccountSet::from_resolved(keys(7)),
            Err(TipAccountError::WrongCount { got: 7 })
        );
        assert_eq!(
            TipAccountSet::from_resolved(keys(9)),
            Err(TipAccountError::WrongCount { got: 9 })
        );
    }

    #[test]
    fn tip_set_rejects_duplicate() {
        let mut k = keys(8);
        k[7] = k[0];
        assert_eq!(
            TipAccountSet::from_resolved(k),
            Err(TipAccountError::DuplicateAccount)
        );
    }

    #[test]
    fn tip_set_rejects_default_placeholder() {
        let mut k = keys(8);
        k[3] = Pubkey::default();
        assert_eq!(
            TipAccountSet::from_resolved(k),
            Err(TipAccountError::DefaultAccount)
        );
    }

    /// A test-only source standing in for the `landing-2` JSON-RPC resolver.
    struct MockSource(Vec<Pubkey>);
    impl TipAccountSource for MockSource {
        fn resolve(&self) -> Result<TipAccountSet, TipAccountError> {
            TipAccountSet::from_resolved(self.0.clone())
        }
    }

    #[test]
    fn source_resolves_oracle_over_eight_accounts() {
        let src = MockSource(keys(8));
        let oracle = src.resolve_oracle(TipParams::default()).unwrap();
        // The oracle round-robins across all 8 runtime-resolved accounts.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..TIP_ACCOUNT_COUNT {
            seen.insert(oracle.next_tip_account());
        }
        assert_eq!(seen.len(), TIP_ACCOUNT_COUNT);
    }

    #[test]
    fn bad_source_fails_oracle_build() {
        // `TipOracle` is not `Debug`, so match the error instead of `unwrap_err`.
        let src = MockSource(keys(3));
        assert!(matches!(
            src.resolve_oracle(TipParams::default()),
            Err(TipAccountError::WrongCount { got: 3 })
        ));
    }

    #[test]
    fn jito_auth_formats_x_jito_auth_header() {
        let auth = JitoAuth::new("11111111-2222-3333-4444-555555555555").unwrap();
        let (name, value) = auth.header();
        assert_eq!(name, JITO_AUTH_HEADER);
        assert_eq!(value, "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn jito_auth_rejects_blank_and_short() {
        assert_eq!(JitoAuth::new("   "), Err(AuthError::Empty));
        assert_eq!(JitoAuth::new("abc"), Err(AuthError::TooShort));
    }

    #[test]
    fn sender_endpoint_mirrors_config() {
        let cfg = SenderConfig {
            url: "https://sender.helius-rpc.com/fast".into(),
            swqos_only: true,
            enabled: true,
        };
        let ep = SenderEndpoint::from_config(&cfg);
        assert_eq!(ep.url, "https://sender.helius-rpc.com/fast");
        assert!(ep.swqos_only && ep.enabled);

        // The probe seam is exercised by a stub; the real HTTP HEAD lands with the Sender client.
        struct AlwaysUp;
        impl EndpointProbe for AlwaysUp {
            fn reachable(&self, _e: &SenderEndpoint) -> bool {
                true
            }
        }
        assert!(AlwaysUp.reachable(&ep));
    }
}
