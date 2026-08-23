//! Where Ferry keeps per-device state, and the FERRY_HOME contract.
//!
//! `FERRY_HOME` (env var) overrides the default `~/.ferry` for ALL
//! per-device state. This is what lets two simulated devices coexist on
//! one machine (scripts/quickstart-e2e.sh): each gets its own home dir,
//! therefore its own identity, therefore its own trust domain.
//!
//! Layout under the home:
//!
//! ```text
//! <home>/identity/device.key   # X25519 identity (ferry-crypto)
//! ```

use std::path::PathBuf;

use crate::error::{CliError, CliResult};

/// Resolve the device home: `$FERRY_HOME` when set (non-empty), else
/// `$HOME/.ferry`. Empty-string FERRY_HOME is treated as unset so a stray
/// `FERRY_HOME= cargo test` behaves like production.
pub fn ferry_home() -> CliResult<PathBuf> {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            CliError::new(
                "no-home",
                "cannot locate a home directory",
                "set HOME, or point FERRY_HOME at a directory to hold Ferry state",
            )
        })?;
    Ok(home.join(".ferry"))
}

/// Directory holding `device.key`.
pub fn identity_root(home: &std::path::Path) -> PathBuf {
    home.join("identity")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ferry_home_env_overrides_home_join() {
        // Safety: tests mutate process env; guard with a mutex if this ever
        // runs in parallel with other env tests (cargo runs them on one
        // thread per binary by default).
        let saved = std::env::var_os("FERRY_HOME");
        std::env::remove_var("FERRY_HOME");
        let fallback = ferry_home().unwrap();
        assert!(fallback.ends_with(".ferry"), "{fallback:?}");

        std::env::set_var("FERRY_HOME", "/tmp/fh-override");
        assert_eq!(ferry_home().unwrap(), PathBuf::from("/tmp/fh-override"));

        // Empty counts as unset.
        std::env::set_var("FERRY_HOME", "");
        let again = ferry_home().unwrap();
        assert!(again.ends_with(".ferry"), "{again:?}");

        match saved {
            Some(v) => std::env::set_var("FERRY_HOME", v),
            None => std::env::remove_var("FERRY_HOME"),
        }
    }
}
