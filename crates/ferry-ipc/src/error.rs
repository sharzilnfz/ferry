//! IPC error types for Ferry.

use thiserror::Error;

/// Errors that can occur during IPC communication, serialization, or framing.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),

    #[error("Deserialization error: {0}")]
    Deserialization(#[source] serde_json::Error),

    #[error("Message too large: {size} bytes exceeds limit of {max} bytes")]
    MessageTooLarge { size: usize, max: usize },

    #[error("Connection closed by peer")]
    ConnectionClosed,

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Timeout: {0}")]
    Timeout(String),
}
