//! `arb-signer` scaffold. The signer sidecar's whole reason to exist is isolation: it holds
//! ONLY the low-balance hot key (§12). For Fase 0 it proves the safety preconditions —
//! refuse to start if the kill-switch file is present, and load the hot key exclusively via
//! `arb_config::secrets::load_hot_keypair`, which rejects any keyfile that is not `0o600`.
//! The signing surface (TxShapeValidator + synchronous PreSignCaps) attaches in Fase 2
//! (implementation-plan §5.7).

use solana_signer::Signer;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let kill_switch = PathBuf::from(
        std::env::var("ARBIT_KILL_SWITCH").unwrap_or_else(|_| "secrets/kill_switch".into()),
    );
    if arb_config::secrets::kill_switch_engaged(&kill_switch) {
        tracing::error!(path = %kill_switch.display(), "kill-switch engaged — signer refuses to start");
        return ExitCode::FAILURE;
    }

    let key_path = PathBuf::from(
        std::env::var("ARBIT_HOT_KEYPAIR").unwrap_or_else(|_| "secrets/hot-keypair.json".into()),
    );
    match arb_config::secrets::load_hot_keypair(&key_path) {
        Ok(kp) => {
            // Never log the secret — only the public key.
            tracing::info!(pubkey = %kp.pubkey(), "signer sidecar up with hot key");
            println!("arb-signer OK — hot pubkey {}", kp.pubkey());
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(%e, "refusing to start: hot key load failed");
            ExitCode::FAILURE
        }
    }
}
