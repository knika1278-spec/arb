//! Parse `infra/config/{program_ids,providers,limits}.toml` into the typed [`ArbConfig`]
//! and run the self-consistency gate [`validate`]. `validate` is the load-bearing check
//! that the TOML cannot drift from the compiled constants, no unverified prop-AMM is ever
//! allowlisted, the tip stays inside the atomic tx, and the active ladder tier has the
//! endpoints it needs. Used by `make config-check` and CI.

use crate::limits;
use crate::program_ids::{self as pid, ProgramIdStatus};
use crate::providers::{DataSourceConfig, LandingConfig};
use serde::Deserialize;
use solana_pubkey::Pubkey;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("toml parse error in {path}: {source}")]
    Toml {
        path: String,
        source: toml::de::Error,
    },
    #[error("invalid pubkey for {name}: {value}")]
    BadPubkey { name: String, value: String },
    #[error("config validation failed: {0}")]
    Invalid(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Cluster {
    #[default]
    MainnetBeta,
    SurfpoolFork,
    LiteSvm,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProgramIdRow {
    pub name: String,
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub verified_on: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UnverifiedRow {
    pub name: String,
    #[serde(default)]
    pub id_prefix: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ProgramIdsConfig {
    #[serde(default)]
    pub wave1: Vec<ProgramIdRow>,
    #[serde(default)]
    pub wave2: Vec<ProgramIdRow>,
    #[serde(default)]
    pub unverified_prop_amm: Vec<UnverifiedRow>,
}

/// Documentation mirror of the compiled hard limits. `validate` asserts each equals the
/// authoritative constant in `limits.rs`.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct LimitsView {
    pub max_tx_account_locks: usize,
    pub max_loaded_accounts: usize,
    pub tx_size_limit_bytes: usize,
    pub max_compute_unit_limit: u32,
    pub base_fee_lamports_per_sig: u64,
    pub cu_limit_sim_margin_bps: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct ProvidersFile {
    #[serde(default)]
    cluster: Cluster,
    data_source: DataSourceConfig,
    landing: LandingConfig,
}

/// Top-level loaded config (single object every module reads).
#[derive(Clone, Debug)]
pub struct ArbConfig {
    pub cluster: Cluster,
    pub program_ids: ProgramIdsConfig,
    pub data_source: DataSourceConfig,
    pub landing: LandingConfig,
    pub limits: LimitsView,
}

impl ArbConfig {
    /// Resolve the landing config (Jito primary, Helius Sender fallback) for the executor.
    pub fn active_landing(&self) -> &LandingConfig {
        &self.landing
    }
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

fn parse_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ConfigError> {
    let text = read(path)?;
    toml::from_str(&text).map_err(|e| ConfigError::Toml {
        path: path.display().to_string(),
        source: e,
    })
}

/// Load + parse the three config files from `config_dir`.
pub fn load(config_dir: impl AsRef<Path>) -> Result<ArbConfig, ConfigError> {
    let dir = config_dir.as_ref();
    let program_ids: ProgramIdsConfig = parse_toml(&dir.join("program_ids.toml"))?;
    let providers: ProvidersFile = parse_toml(&dir.join("providers.toml"))?;
    let limits: LimitsView = parse_toml(&dir.join("limits.toml"))?;
    Ok(ArbConfig {
        cluster: providers.cluster,
        program_ids,
        data_source: providers.data_source,
        landing: providers.landing,
        limits,
    })
}

fn parse_pk(name: &str, value: &str) -> Result<Pubkey, ConfigError> {
    value.parse::<Pubkey>().map_err(|_| ConfigError::BadPubkey {
        name: name.to_string(),
        value: value.to_string(),
    })
}

/// The self-consistency gate. Any failure is a hard CI error.
pub fn validate(cfg: &ArbConfig) -> Result<(), ConfigError> {
    let inv = |m: String| ConfigError::Invalid(m);

    // 1. Tip must live inside the atomic tx (fail => tip unpaid).
    if !cfg.landing.jito.tip_inside_tx {
        return Err(inv(
            "landing.jito.tip_inside_tx must be true (tip inside atomic tx)".into(),
        ));
    }

    // 2. Limits TOML must equal the compiled authoritative constants.
    let lv = &cfg.limits;
    let mismatches = [
        (
            lv.max_tx_account_locks == limits::MAX_TX_ACCOUNT_LOCKS,
            "max_tx_account_locks",
        ),
        (
            lv.max_loaded_accounts == limits::MAX_LOADED_ACCOUNTS,
            "max_loaded_accounts",
        ),
        (
            lv.tx_size_limit_bytes == limits::TX_SIZE_LIMIT_BYTES,
            "tx_size_limit_bytes",
        ),
        (
            lv.max_compute_unit_limit == limits::MAX_COMPUTE_UNIT_LIMIT,
            "max_compute_unit_limit",
        ),
        (
            lv.base_fee_lamports_per_sig == limits::BASE_FEE_LAMPORTS_PER_SIG,
            "base_fee_lamports_per_sig",
        ),
        (
            lv.cu_limit_sim_margin_bps == limits::CU_LIMIT_SIM_MARGIN_BPS,
            "cu_limit_sim_margin_bps",
        ),
    ];
    for (ok, field) in mismatches {
        if !ok {
            return Err(inv(format!("limits.toml::{field} != compiled constant")));
        }
    }

    // 3. Wave-1 rows: status verified, id matches the compiled const table, allowlisted.
    for row in &cfg.program_ids.wave1 {
        if row.status != "verified" {
            return Err(inv(format!("wave1 '{}' must be status=verified", row.name)));
        }
        let pk = parse_pk(&row.name, &row.id)?;
        let entry = pid::PROGRAM_ID_TABLE
            .iter()
            .find(|e| e.name == row.name)
            .ok_or_else(|| {
                inv(format!(
                    "wave1 '{}' not present in compiled PROGRAM_ID_TABLE",
                    row.name
                ))
            })?;
        if entry.id != pk {
            return Err(inv(format!("wave1 '{}' id != compiled const id", row.name)));
        }
        if !pid::is_allowlisted_swap_program(&pk) {
            return Err(inv(format!(
                "wave1 '{}' id not in WAVE1_DEX_ALLOWLIST",
                row.name
            )));
        }
        if !matches!(entry.status, ProgramIdStatus::Verified { .. }) {
            return Err(inv(format!(
                "compiled status for '{}' is not Verified",
                row.name
            )));
        }
    }

    // 4. Wave-2 rows must NOT be allowlisted.
    for row in &cfg.program_ids.wave2 {
        let pk = parse_pk(&row.name, &row.id)?;
        if pid::is_allowlisted_swap_program(&pk) {
            return Err(inv(format!("wave2 '{}' must not be allowlisted", row.name)));
        }
    }

    // 5. No unverified prop-AMM may ever be allowlisted. (They carry only prefixes here;
    //    if a full id was supplied and parses, it must not be in the allowlist.)
    for row in &cfg.program_ids.unverified_prop_amm {
        if let Some(prefix) = &row.unverified_full_id() {
            if let Ok(pk) = prefix.parse::<Pubkey>() {
                if pid::is_allowlisted_swap_program(&pk) {
                    return Err(inv(format!(
                        "unverified prop-AMM '{}' is allowlisted!",
                        row.name
                    )));
                }
            }
        }
    }

    // 6. Active tier must have the endpoints it needs.
    if cfg.data_source.active_tier.requires_grpc() && cfg.data_source.grpc.is_none() {
        return Err(inv(format!(
            "active tier {:?} requires a yellowstone grpc endpoint but none configured",
            cfg.data_source.active_tier
        )));
    }

    // 7. If an env-var name is declared for the secret JSON-RPC/WSS URL, it must be a non-empty
    //    name (the URL value itself lives in the env, never here). Catches a blank indirection.
    for (field, name) in [
        ("json_rpc_url_env", &cfg.data_source.json_rpc_url_env),
        (
            "fallback_wss_url_env",
            &cfg.data_source.fallback_wss_url_env,
        ),
    ] {
        if let Some(n) = name {
            if n.trim().is_empty() {
                return Err(inv(format!("data_source.{field} is declared but empty")));
            }
        }
    }

    // 8. landing-1: the Jito auth indirection + the pinned landing endpoints must be present. The
    //    UUID *value* lives in the env named by `auth_uuid_env` (never here), but a BLANK
    //    indirection would silently drop the x-jito-auth header — reject it at load. The Block
    //    Engine + Helius Sender URLs are non-secret and must be pinned, not empty.
    if cfg.landing.jito.auth_uuid_env.trim().is_empty() {
        return Err(inv(
            "landing.jito.auth_uuid_env is empty (names the env var holding the x-jito-auth UUID)"
                .into(),
        ));
    }
    for (field, url) in [
        ("jito.block_engine_url", &cfg.landing.jito.block_engine_url),
        ("helius_sender.url", &cfg.landing.helius_sender.url),
    ] {
        if url.trim().is_empty() {
            return Err(inv(format!("landing.{field} must be a non-empty endpoint")));
        }
    }

    Ok(())
}

impl UnverifiedRow {
    /// A prop-AMM row only carries a (possibly partial) prefix; treat it as a full id only
    /// if it is plausibly base58-length. Always returns Some(prefix) so step-5 can probe it.
    fn unverified_full_id(&self) -> Option<String> {
        self.id_prefix.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{
        Commitment, DataSourceConfig, GrpcEndpoint, JitoConfig, LadderTier, LandingConfig,
        SenderConfig,
    };

    fn base_cfg() -> ArbConfig {
        ArbConfig {
            cluster: Cluster::MainnetBeta,
            program_ids: ProgramIdsConfig {
                wave1: vec![
                    ProgramIdRow {
                        name: "raydium_cpmm".into(),
                        id: pid::RAYDIUM_CPMM.to_string(),
                        status: "verified".into(),
                        verified_on: None,
                    },
                    ProgramIdRow {
                        name: "orca_whirlpool".into(),
                        id: pid::ORCA_WHIRLPOOL.to_string(),
                        status: "verified".into(),
                        verified_on: None,
                    },
                    ProgramIdRow {
                        name: "pumpswap_amm".into(),
                        id: pid::PUMPSWAP_AMM.to_string(),
                        status: "verified".into(),
                        verified_on: None,
                    },
                ],
                wave2: vec![ProgramIdRow {
                    name: "raydium_amm_v4".into(),
                    id: pid::RAYDIUM_AMM_V4.to_string(),
                    status: "deferred_wave2".into(),
                    verified_on: None,
                }],
                unverified_prop_amm: vec![UnverifiedRow {
                    name: "humidifi".into(),
                    id_prefix: Some("9H6tu".into()),
                }],
            },
            data_source: DataSourceConfig {
                active_tier: LadderTier::FirstProfit,
                grpc: Some(GrpcEndpoint {
                    url: "https://grpc.example".into(),
                    token_env: "GRPC_TOKEN".into(),
                    commitment: Commitment::Processed,
                    max_streams: 2,
                }),
                fallback_wss: None,
                json_rpc: "https://rpc.example".into(),
                json_rpc_url_env: None,
                fallback_wss_url_env: None,
                shredstream: None,
            },
            landing: LandingConfig {
                jito: JitoConfig {
                    block_engine_url: "https://frankfurt.jito".into(),
                    auth_uuid_env: "JITO_UUID".into(),
                    tip_inside_tx: true,
                    tip_cap_bps: 5000,
                },
                helius_sender: SenderConfig {
                    url: "https://sender.helius".into(),
                    swqos_only: false,
                    enabled: true,
                },
            },
            limits: LimitsView {
                max_tx_account_locks: limits::MAX_TX_ACCOUNT_LOCKS,
                max_loaded_accounts: limits::MAX_LOADED_ACCOUNTS,
                tx_size_limit_bytes: limits::TX_SIZE_LIMIT_BYTES,
                max_compute_unit_limit: limits::MAX_COMPUTE_UNIT_LIMIT,
                base_fee_lamports_per_sig: limits::BASE_FEE_LAMPORTS_PER_SIG,
                cu_limit_sim_margin_bps: limits::CU_LIMIT_SIM_MARGIN_BPS,
            },
        }
    }

    #[test]
    fn base_config_validates() {
        validate(&base_cfg()).expect("base config should validate");
    }

    #[test]
    fn rejects_tip_outside_tx() {
        let mut c = base_cfg();
        c.landing.jito.tip_inside_tx = false;
        assert!(validate(&c).is_err());
    }

    #[test]
    fn rejects_limits_drift() {
        let mut c = base_cfg();
        c.limits.max_tx_account_locks = 256; // the classic wrong value
        assert!(validate(&c).is_err());
    }

    #[test]
    fn rejects_grpc_missing_for_paid_tier() {
        let mut c = base_cfg();
        c.data_source.grpc = None;
        assert!(validate(&c).is_err());
    }

    #[test]
    fn build_proof_tier_allows_no_grpc() {
        let mut c = base_cfg();
        c.data_source.active_tier = LadderTier::BuildProof;
        c.data_source.grpc = None;
        validate(&c).expect("build-proof tier needs no grpc");
    }

    #[test]
    fn rejects_wave1_id_mismatch() {
        let mut c = base_cfg();
        c.program_ids.wave1[0].id = pid::ORCA_WHIRLPOOL.to_string(); // wrong id for the name
        assert!(validate(&c).is_err());
    }

    #[test]
    fn rejects_blank_jito_auth_uuid_env() {
        // landing-1: a blank indirection would silently drop the x-jito-auth header.
        let mut c = base_cfg();
        c.landing.jito.auth_uuid_env = "  ".into();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn rejects_empty_landing_endpoint() {
        let mut c = base_cfg();
        c.landing.helius_sender.url = String::new();
        assert!(validate(&c).is_err());
    }
}
