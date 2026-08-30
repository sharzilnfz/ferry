

use thiserror::Error;


#[derive(Debug, Error)]
pub enum TuiError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IPC error: {0}")]
    Ipc(#[from] ferry_ipc::IpcError),

    #[error("Terminal error: {0}")]
    Terminal(String),
}
