//! One owner for folder bootstrap and the payload-file pairing ritual.
//!
//! Two concerns live here, and only here (ticket 08):
//!
//! - **Folder bootstrap** ([`folder`]): everything under `<folder>/.ferry/` —
//!   open / create / adopt, settings access, polynomial lookup.
//! - **The pairing ritual** ([`pairing`]): write offer, poll for response,
//!   complete the transcript MAC, wrap the FMK for the peer, append the wrap
//!   entry to `CONFIG_HEAD`, seal the grant.
//!
//! Both return plain structs and coded [`FolderError`]s. No QR rendering,
//! no CLI output shaping, no JSON documents: frontends (`ferry-cli`,
//! `ferry-daemon`) map results to their own presentation.

pub mod error;
pub mod folder;
pub mod pairing;

pub use error::{CodeInto, FolderError, FolderResult};
