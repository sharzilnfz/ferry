use std::path::PathBuf;

use crate::error::{CliError, CliResult, CodeInto};

pub fn ferry_home() -> CliResult<PathBuf> {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
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

pub fn identity_root(home: &std::path::Path) -> PathBuf {
    home.join("identity")
}

pub fn load_device_identity() -> CliResult<ferry_crypto::identity::DeviceIdentity> {
    let home = ferry_home()?;
    ferry_crypto::identity::load_or_create(&identity_root(&home))
        .code("identity", "cannot read or create the device identity key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ferry_home_env_overrides_home_join() {
        let saved = std::env::var_os("FERRY_HOME");
        std::env::remove_var("FERRY_HOME");
        let fallback = ferry_home().unwrap();
        assert!(fallback.ends_with(".ferry"), "{fallback:?}");

        std::env::set_var("FERRY_HOME", "/tmp/fh-override");
        assert_eq!(ferry_home().unwrap(), PathBuf::from("/tmp/fh-override"));

        std::env::set_var("FERRY_HOME", "");
        let again = ferry_home().unwrap();
        assert!(again.ends_with(".ferry"), "{again:?}");

        match saved {
            Some(v) => std::env::set_var("FERRY_HOME", v),
            None => std::env::remove_var("FERRY_HOME"),
        }
    }
}
