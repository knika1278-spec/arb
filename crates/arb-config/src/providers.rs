//! Typed model of the data-source + landing ladder (plan.md §8). Loaded from
//! `infra/config/providers.toml`. Secrets (auth tokens) are NEVER stored here — each
//! endpoint names the *env var* that holds its token.

use serde::Deserialize;
use std::net::SocketAddr;

/// URLs are kept as validated-on-use strings to avoid an extra dependency.
pub type Url = String;

/// Cost-ladder tier (plan.md §8). Drives which endpoints are required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LadderTier {
    /// Fase 0-1: Jito ShredStream (free) + free WSS; no paid gRPC. ~$0-40/mo.
    BuildProof,
    /// Fase 2 DEFAULT: Chainstack Growth + Yellowstone gRPC add-on. ~$98-198/mo.
    #[default]
    FirstProfit,
    /// Fase 3: owner-firehose discovery; self-host or Chainstack flat add-on.
    NicheFirehose,
    /// Fase 4: dedicated/co-located for liquid-pair races.
    Competitive,
}

impl LadderTier {
    /// Whether this tier requires a paid Yellowstone gRPC endpoint to function.
    pub fn requires_grpc(self) -> bool {
        !matches!(self, LadderTier::BuildProof)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    #[default]
    Processed,
    Confirmed,
    Finalized,
}

/// Yellowstone gRPC endpoint. `token_env` names the env var holding the auth token.
#[derive(Clone, Debug, Deserialize)]
pub struct GrpcEndpoint {
    pub url: Url,
    pub token_env: String,
    #[serde(default)]
    pub commitment: Commitment,
    #[serde(default = "default_max_streams")]
    pub max_streams: u8,
}
fn default_max_streams() -> u8 {
    2
}

/// Jito ShredStream proxy — free across all tiers (sub-slot tx-intent).
#[derive(Clone, Debug, Deserialize)]
pub struct ShredStreamConfig {
    pub proxy_addr: SocketAddr,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DataSourceConfig {
    #[serde(default)]
    pub active_tier: LadderTier,
    #[serde(default)]
    pub grpc: Option<GrpcEndpoint>,
    #[serde(default)]
    pub fallback_wss: Option<Url>,
    /// Non-secret default/fallback JSON-RPC endpoint (e.g. the public mainnet RPC). The real
    /// Chainstack Solana endpoint carries its key *inside the URL*, so it is NEVER stored here
    /// — `json_rpc_url_env` names the env var that holds it and overrides this at resolve time.
    pub json_rpc: Url,
    /// Names the env var holding the full secret Chainstack HTTPS RPC URL
    /// (`https://nd-xxx.p2pify.com/<KEY>`). When set & non-empty it overrides `json_rpc`. Only
    /// the env-var *name* lives in config, so the secret never enters the committed TOML nor the
    /// `Debug`-printable [`crate::loader::ArbConfig`] — mirroring [`GrpcEndpoint::token_env`].
    #[serde(default)]
    pub json_rpc_url_env: Option<String>,
    /// Names the env var holding the full secret Chainstack WSS URL. Overrides `fallback_wss`.
    #[serde(default)]
    pub fallback_wss_url_env: Option<String>,
    #[serde(default)]
    pub shredstream: Option<ShredStreamConfig>,
}

impl DataSourceConfig {
    /// Effective JSON-RPC endpoint: the secret URL from the env var named by `json_rpc_url_env`
    /// if that var is set & non-empty, else the non-secret `json_rpc` default. Call this at the
    /// RPC-client boundary so the key is read from the environment on use and is never held in
    /// the logged config.
    pub fn resolve_json_rpc(&self) -> Url {
        override_from(self.json_rpc_url_env.as_deref(), |n| std::env::var(n).ok())
            .unwrap_or_else(|| self.json_rpc.clone())
    }

    /// Effective WSS endpoint: the secret URL from the env var named by `fallback_wss_url_env`
    /// if set & non-empty, else the non-secret `fallback_wss` default (which may be `None`).
    pub fn resolve_fallback_wss(&self) -> Option<Url> {
        override_from(self.fallback_wss_url_env.as_deref(), |n| {
            std::env::var(n).ok()
        })
        .or_else(|| self.fallback_wss.clone())
    }
}

/// Resolve an env-var-name override: returns the looked-up value (trimmed) only if a name was
/// given AND the value is present and non-empty. `lookup` is injected so the resolution logic is
/// unit-testable without mutating the process environment.
fn override_from(var_name: Option<&str>, lookup: impl Fn(&str) -> Option<String>) -> Option<Url> {
    lookup(var_name?)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The HTTP header the Jito Block Engine reads for the per-account allowlisted UUID rate-limit
/// (landing-1). The UUID *value* never lives in committed config — it is resolved from the env
/// var named by [`JitoConfig::auth_uuid_env`] at use, mirroring the URL-embedded RPC secret.
pub const JITO_AUTH_HEADER: &str = "x-jito-auth";

/// Jito Block-Engine landing config. `tip_inside_tx` is a hard invariant (validated true).
#[derive(Clone, Debug, Deserialize)]
pub struct JitoConfig {
    pub block_engine_url: Url,
    pub auth_uuid_env: String,
    /// MUST be true: tip transfer lives inside the atomic arb tx so a revert => tip unpaid.
    #[serde(default = "default_true")]
    pub tip_inside_tx: bool,
    /// Tip is capped as this fraction (bps) of simulated profit.
    #[serde(default = "default_tip_cap_bps")]
    pub tip_cap_bps: u32,
}
fn default_true() -> bool {
    true
}
fn default_tip_cap_bps() -> u32 {
    5_000 // 50% ceiling per plan.md §9/§10
}

impl JitoConfig {
    /// Resolve the allowlisted Jito UUID from the env var named by `auth_uuid_env`. Returns `None`
    /// when the var is unset/blank (operator has not provisioned the account yet), in which case
    /// the executor falls back to the un-authenticated public rate-limit. Read at the request
    /// boundary so the secret stays out of the `Debug`-printable [`crate::loader::ArbConfig`],
    /// exactly as [`DataSourceConfig::resolve_json_rpc`] keeps the RPC key out.
    pub fn resolve_auth_uuid(&self) -> Option<String> {
        override_from(Some(self.auth_uuid_env.as_str()), |n| std::env::var(n).ok())
    }

    /// The `(x-jito-auth, <uuid>)` header pair for the Block Engine JSON-RPC client, or `None`
    /// when no UUID is provisioned. The client attaches it to every `sendBundle`/`getTipAccounts`.
    pub fn auth_header(&self) -> Option<(&'static str, String)> {
        auth_header_pair(self.resolve_auth_uuid())
    }
}

/// Pure header-formatting step, split out so it is unit-testable without touching the process
/// environment (the env read lives in [`JitoConfig::resolve_auth_uuid`]).
fn auth_header_pair(uuid: Option<String>) -> Option<(&'static str, String)> {
    uuid.map(|u| (JITO_AUTH_HEADER, u))
}

/// Helius Sender fallback (dual-route SWQoS + Jito, 0 credits).
#[derive(Clone, Debug, Deserialize)]
pub struct SenderConfig {
    pub url: Url,
    #[serde(default)]
    pub swqos_only: bool,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LandingConfig {
    pub jito: JitoConfig,
    pub helius_sender: SenderConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_prefers_env_when_present_and_nonempty() {
        let got = override_from(Some("ANY"), |_| {
            Some("https://nd-1-2-3.p2pify.com/KEY".into())
        });
        assert_eq!(got.as_deref(), Some("https://nd-1-2-3.p2pify.com/KEY"));
    }

    #[test]
    fn override_is_none_without_a_var_name() {
        assert_eq!(
            override_from(None, |_| Some("should-be-ignored".into())),
            None
        );
    }

    #[test]
    fn override_is_none_when_env_missing() {
        assert_eq!(override_from(Some("ANY"), |_| None), None);
    }

    #[test]
    fn override_is_none_when_env_blank() {
        assert_eq!(override_from(Some("ANY"), |_| Some("   ".into())), None);
    }

    #[test]
    fn override_trims_surrounding_whitespace() {
        let got = override_from(Some("ANY"), |_| {
            Some("  wss://ws-nd.p2pify.com/KEY \n".into())
        });
        assert_eq!(got.as_deref(), Some("wss://ws-nd.p2pify.com/KEY"));
    }

    #[test]
    fn resolve_falls_back_to_toml_defaults_when_env_unset() {
        // A var name that is not set in the test environment must yield the non-secret defaults,
        // proving the build stays runnable on public RPC until a real `.env` is provisioned.
        let c = DataSourceConfig {
            active_tier: LadderTier::default(),
            grpc: None,
            fallback_wss: Some("wss://api.mainnet-beta.solana.com".into()),
            json_rpc: "https://api.mainnet-beta.solana.com".into(),
            json_rpc_url_env: Some("ARBIT_TEST_UNSET_RPC_VAR_DO_NOT_DEFINE".into()),
            fallback_wss_url_env: Some("ARBIT_TEST_UNSET_WSS_VAR_DO_NOT_DEFINE".into()),
            shredstream: None,
        };
        assert_eq!(c.resolve_json_rpc(), "https://api.mainnet-beta.solana.com");
        assert_eq!(
            c.resolve_fallback_wss().as_deref(),
            Some("wss://api.mainnet-beta.solana.com")
        );
    }

    #[test]
    fn auth_header_pair_uses_x_jito_auth_when_uuid_present() {
        let got = auth_header_pair(Some("3f1c…allowlisted-uuid".into()));
        assert_eq!(
            got,
            Some((JITO_AUTH_HEADER, "3f1c…allowlisted-uuid".to_string()))
        );
        assert_eq!(JITO_AUTH_HEADER, "x-jito-auth");
    }

    #[test]
    fn auth_header_pair_is_none_without_uuid() {
        // No provisioned UUID => no header (executor runs on the public rate-limit until provisioned).
        assert_eq!(auth_header_pair(None), None);
    }
}
