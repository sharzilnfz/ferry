//! ferry-cli: the `ferry` binary. Library form exists so integration tests
//! exercise the exact command logic without spawning processes.

pub mod cli;
pub mod commands;
pub mod error;
pub mod exchange;
pub mod folder;
pub mod home;
pub mod out;
pub mod scan;

/// Resolve the device identity under the resolved `FERRY_HOME`, creating it on
/// first use. Shared by several commands.
pub fn ensure_identity() -> error::CliResult<ferry_crypto::identity::DeviceIdentity> {
    let home = home::ferry_home()?;
    ferry_crypto::identity::load_or_create(&home::identity_root(&home)).map_err(|e| {
        error::CliError::new(
            "identity-corrupt",
            e.to_string(),
            "your device.key is damaged; restore it from backup or delete it deliberately (this forks trust)",
        )
    })
}
