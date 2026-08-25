//! Subcommand implementations. Each `run*` maps parsed args onto explicit
//! parameters so tests call the logic directly.

pub mod conflicts;
pub mod daemon;
pub mod ignore_cmd;
pub mod init;
pub mod pairing;
pub mod pin;
pub mod share;
pub mod status;
pub mod store;
pub mod sync;
