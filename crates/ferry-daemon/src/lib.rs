//! `ferry-daemon`: Local sync daemon with typed IPC server and headless operation.

pub mod ipc;
pub mod state;
pub mod timefmt;
pub mod ui;

pub use ipc::{dispatch_client_command, handle_client_connection, spawn_ipc_server, IpcServerHandle};
pub use state::DaemonState;
