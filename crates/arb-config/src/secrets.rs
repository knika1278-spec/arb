//! The ONLY sanctioned hot-key loader + the kill-switch probe. Enforces the key-mgmt
//! invariant (§12) at load time: a hot keypair file whose unix mode is not `0o600` is
//! refused. Treasury/upgrade authority is never loaded here (KMS + Squads multisig).

use solana_keypair::{read_keypair_file, Keypair};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("hot key file not found: {0}")]
    NotFound(String),
    #[error("hot key file {path} has unsafe permissions {mode:#o}; must be 0o600")]
    UnsafePermissions { path: String, mode: u32 },
    #[error("failed to read hot keypair: {0}")]
    Read(String),
}

/// Load the low-balance hot keypair the signer sidecar uses. Refuses a file that is not
/// `0o600` on unix (group/other readability is a key-exfil vector). On non-unix the perm
/// check is skipped with a tracing warning (Windows ACLs differ; dev only — never run a
/// funded key on a non-hardened host).
pub fn load_hot_keypair(path: &Path) -> Result<Keypair, SecretError> {
    if !path.exists() {
        return Err(SecretError::NotFound(path.display().to_string()));
    }
    enforce_owner_only_perms(path)?;
    read_keypair_file(path).map_err(|e| SecretError::Read(e.to_string()))
}

#[cfg(unix)]
fn enforce_owner_only_perms(path: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| SecretError::Read(e.to_string()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(SecretError::UnsafePermissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only_perms(path: &Path) -> Result<(), SecretError> {
    // No std dependency on a logger here; the bot/signer log this at startup. On Windows the
    // unix 0o600 mode check is not applicable (ACLs differ) — never run a funded key here.
    eprintln!(
        "WARN: skipping 0o600 hot-key permission check on non-unix host ({}) — do NOT run a funded key here",
        path.display()
    );
    Ok(())
}

/// Returns true if the kill-switch file is present (presence => halt all signing).
pub fn kill_switch_engaged(path: &Path) -> bool {
    path.exists()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn rejects_group_readable_key() {
        let dir = std::env::temp_dir().join(format!("arbsec_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hot-keypair.json");
        let mut f = std::fs::File::create(&p).unwrap();
        // a syntactically-valid keypair byte array (64 zero bytes)
        write!(f, "{:?}", vec![0u8; 64]).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_hot_keypair(&p),
            Err(SecretError::UnsafePermissions { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kill_switch_presence() {
        let dir = std::env::temp_dir().join(format!("arbks_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("kill_switch");
        assert!(!kill_switch_engaged(&p));
        std::fs::File::create(&p).unwrap();
        assert!(kill_switch_engaged(&p));
        std::fs::remove_dir_all(&dir).ok();
    }
}
