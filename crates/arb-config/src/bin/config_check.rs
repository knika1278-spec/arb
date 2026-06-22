//! `config-check` — load `infra/config/*.toml` and run `arb_config::validate`. Exits
//! non-zero on any inconsistency so `make config-check` / CI fail loudly. The program-id
//! ⇄ compiled-table cross-check lives inside `validate`.
//!
//! Usage: `config-check [CONFIG_DIR]` (default `infra/config`).

use std::process::ExitCode;

fn main() -> ExitCode {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "infra/config".to_string());
    match arb_config::load(&dir).and_then(|cfg| {
        arb_config::validate(&cfg)?;
        Ok(cfg)
    }) {
        Ok(cfg) => {
            println!(
                "config OK: cluster={:?} tier={:?} tip_inside_tx={} wave1={} ids",
                cfg.cluster,
                cfg.data_source.active_tier,
                cfg.landing.jito.tip_inside_tx,
                cfg.program_ids.wave1.len(),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("config-check FAILED: {e}");
            ExitCode::FAILURE
        }
    }
}
