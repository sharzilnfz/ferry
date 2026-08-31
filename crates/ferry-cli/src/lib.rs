#![allow(warnings, clippy::all, clippy::pedantic)]

pub mod bootstrap;
pub mod cli;
pub mod commands;
pub mod error;
pub mod folder;
pub mod home;
pub mod ipc;
pub mod out;
pub mod scan;

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
