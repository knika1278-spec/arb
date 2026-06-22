//! `arb-bot` scaffold entrypoint. For Fase 0 this proves the config/secrets contract
//! end-to-end: load `ArbConfig`, refuse to run if the kill-switch is engaged, and print the
//! resolved ladder tier + landing route. The detection→sizing→txbuilder→signer→executor
//! pipeline modules attach here as they land (implementation-plan §5.4–§5.9).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_dir = std::env::var("ARBIT_CONFIG_DIR").unwrap_or_else(|_| "infra/config".into());

    let cfg = match arb_config::load(&config_dir) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(%e, "failed to load config");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = arb_config::validate(&cfg) {
        tracing::error!(%e, "config validation failed");
        return ExitCode::FAILURE;
    }

    // Kill-switch gate (presence => halt). Path is conventionally secrets/kill_switch.
    let kill_switch = PathBuf::from(
        std::env::var("ARBIT_KILL_SWITCH").unwrap_or_else(|_| "secrets/kill_switch".into()),
    );
    if arb_config::secrets::kill_switch_engaged(&kill_switch) {
        tracing::error!(path = %kill_switch.display(), "kill-switch engaged — refusing to start");
        return ExitCode::FAILURE;
    }

    tracing::info!(
        cluster = ?cfg.cluster,
        tier = ?cfg.data_source.active_tier,
        tip_inside_tx = cfg.landing.jito.tip_inside_tx,
        "arb-bot scaffold up; wave-1 allowlist has {} venues",
        arb_config::WAVE1_DEX_ALLOWLIST.len()
    );
    println!(
        "arb-bot OK — cluster={:?} tier={:?} landing=jito:{} sender:{}",
        cfg.cluster,
        cfg.data_source.active_tier,
        cfg.landing.jito.block_engine_url,
        cfg.landing.helius_sender.url
    );
    ExitCode::SUCCESS
}
