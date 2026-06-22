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
    /// Non-secret default/placeholder gRPC host. The REAL Chainstack Yellowstone host
    /// (`<chain>-mainnet.core.chainstack.com:443`, port 443) is console-only (Node overview → gRPC
    /// endpoint), so it is NEVER committed here — `url_env` names the env var that holds it and
    /// overrides this at resolve time, mirroring [`DataSourceConfig::json_rpc_url_env`].
    pub url: Url,
    /// Names the env var holding the full secret Chainstack Yellowstone gRPC endpoint. When set &
    /// non-empty it overrides `url` (see [`GrpcEndpoint::resolve_url`]). Only the env-var *name*
    /// lives in config, so the real per-node host never enters the committed TOML nor the
    /// `Debug`-printable [`crate::loader::ArbConfig`] — mirroring [`DataSourceConfig::json_rpc_url_env`].
    #[serde(default)]
    pub url_env: Option<String>,
    /// Names the env var holding the gRPC auth token. Chainstack gRPC auth is the **`x-token`
    /// metadata header** (resolved at the client boundary), NOT a URL-embedded secret like the
    /// JSON-RPC endpoint — so the token stays separate from `url`/`url_env`.
    pub token_env: String,
    #[serde(default)]
    pub commitment: Commitment,
    #[serde(default = "default_max_streams")]
    pub max_streams: u8,
}
fn default_max_streams() -> u8 {
    2
}

impl GrpcEndpoint {
    /// Effective gRPC endpoint: the secret host from the env var named by `url_env` if that var is
    /// set & non-empty, else the non-secret `url` default. Call this at the gRPC-client boundary so
    /// the real host is read from the environment on use and is never held in the logged config.
    /// The auth token is resolved separately from `token_env` (Chainstack gRPC = `x-token` header).
    pub fn resolve_url(&self) -> Url {
        override_from(self.url_env.as_deref(), |n| std::env::var(n).ok())
            .unwrap_or_else(|| self.url.clone())
    }
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
    /// Names the env var holding the HTTP **Basic-Auth username** for a Chainstack node that uses
    /// the username/password credential set instead of the key-in-path form (a node exposes BOTH;
    /// `https://<bare-host>` + Basic Auth, or `https://nd-xxx.p2pify.com/<KEY>`). Resolved via
    /// [`DataSourceConfig::resolve_basic_auth`] at the client boundary; the secret value lives in
    /// the env, never here. `None` when the endpoint already carries its own credentials in the URL.
    #[serde(default)]
    pub basic_auth_user_env: Option<String>,
    /// Names the env var holding the HTTP **Basic-Auth password** (pairs with `basic_auth_user_env`).
    #[serde(default)]
    pub basic_auth_pass_env: Option<String>,
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

    /// Effective Basic-Auth credentials `(username, password)` for a node that authenticates with
    /// the username/password set rather than a key-in-path URL. Returns `Some` only when BOTH env
    /// vars named by `basic_auth_user_env` / `basic_auth_pass_env` are declared and resolve to
    /// non-empty values; otherwise `None` (the endpoint carries its own auth in the URL, or this is
    /// the public default). The RPC/gRPC client applies these as an `Authorization: Basic` header
    /// (preferred — avoids URL-encoding the secret); Surfpool's `--rpc-url` can instead embed them
    /// as `https://user:pass@host`. Read at the client boundary so the secret never enters the
    /// `Debug`-printable [`crate::loader::ArbConfig`].
    pub fn resolve_basic_auth(&self) -> Option<(String, String)> {
        let user = override_from(self.basic_auth_user_env.as_deref(), |n| {
            std::env::var(n).ok()
        })?;
        let pass = override_from(self.basic_auth_pass_env.as_deref(), |n| {
            std::env::var(n).ok()
        })?;
        Some((user, pass))
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
            basic_auth_user_env: None,
            basic_auth_pass_env: None,
            shredstream: None,
        };
        assert_eq!(c.resolve_json_rpc(), "https://api.mainnet-beta.solana.com");
        assert_eq!(
            c.resolve_fallback_wss().as_deref(),
            Some("wss://api.mainnet-beta.solana.com")
        );
        // No basic-auth env names declared ⇒ no credentials resolved (public default path).
        assert_eq!(c.resolve_basic_auth(), None);
    }

    #[test]
    fn resolve_basic_auth_needs_both_names_and_values() {
        // Only the username name declared ⇒ None (a half-configured Basic-Auth pair must not
        // silently authenticate with an empty password).
        let mut c = DataSourceConfig {
            active_tier: LadderTier::default(),
            grpc: None,
            fallback_wss: None,
            json_rpc: "https://solana-mainnet.core.chainstack.com".into(),
            json_rpc_url_env: None,
            fallback_wss_url_env: None,
            basic_auth_user_env: Some("ARBIT_TEST_UNSET_USER_DO_NOT_DEFINE".into()),
            basic_auth_pass_env: None,
            shredstream: None,
        };
        assert_eq!(c.resolve_basic_auth(), None);
        // Both names declared but the env vars are unset in the test process ⇒ still None.
        c.basic_auth_pass_env = Some("ARBIT_TEST_UNSET_PASS_DO_NOT_DEFINE".into());
        assert_eq!(c.resolve_basic_auth(), None);
    }

    #[test]
    fn grpc_resolve_url_falls_back_to_placeholder_when_env_unset() {
        // A declared-but-unset url_env must yield the non-secret placeholder host, so the config
        // still resolves a value pre-provisioning (the real host arrives via the env at runtime).
        let g = GrpcEndpoint {
            url: "https://yellowstone.chainstack.example:443".into(),
            url_env: Some("ARBIT_TEST_UNSET_GRPC_VAR_DO_NOT_DEFINE".into()),
            token_env: "CHAINSTACK_GRPC_TOKEN".into(),
            commitment: Commitment::Processed,
            max_streams: 2,
        };
        assert_eq!(
            g.resolve_url(),
            "https://yellowstone.chainstack.example:443"
        );
    }

    #[test]
    fn grpc_resolve_url_uses_placeholder_when_no_url_env_declared() {
        let g = GrpcEndpoint {
            url: "https://yellowstone.chainstack.example:443".into(),
            url_env: None,
            token_env: "CHAINSTACK_GRPC_TOKEN".into(),
            commitment: Commitment::Processed,
            max_streams: 2,
        };
        assert_eq!(
            g.resolve_url(),
            "https://yellowstone.chainstack.example:443"
        );
    }
}
