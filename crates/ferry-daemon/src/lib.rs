pub mod device_daemon;
pub mod folder_engine;
pub mod ipc;
pub mod state;
pub mod supervisor;
pub mod ui;

pub use folder_engine::FolderEngine;
pub use ipc::{
    dispatch_client_command, handle_client_connection, spawn_ipc_server, IpcServerHandle,
};
pub use state::DaemonState;
pub use supervisor::{FolderId, Supervisor};

pub mod timefmt {
    pub use ferry_platform::time::*;
}
