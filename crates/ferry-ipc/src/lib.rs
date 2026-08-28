//! `ferry-ipc`: Lightweight local IPC wire protocol, framing, and transports for Ferry.
//!
//! Provides typed newline-delimited JSON messaging over Unix domain sockets,
//! Windows named pipes, and in-memory duplex streams.

pub mod backend;
pub mod error;
pub mod framing;
pub mod paths;
pub mod protocol;
pub mod transport;

pub use backend::{
    BoxFuture, DirectoryListing, FakeBackend, FolderInfo, FsEntry, OpError, PairResult,
    PairingSession, PinRecord, PinReleaseSummary, PinStopSummary, ShareOffer, ShareStatus,
    UiBackend, UiEvent, UiEventStream,
};
pub use error::IpcError;
pub use framing::{IpcConnection, IpcReceiver, IpcSender, DEFAULT_MAX_MESSAGE_SIZE};
pub use paths::{
    default_socket_path, socket_path_for_dir, DEFAULT_SOCKET_FILENAME, DEFAULT_WINDOWS_PIPE_PREFIX,
};
pub use protocol::{
    ClientCommand, ConflictEntry, DaemonMessage, DeviceStamp, EngineSnapshot, PeerStatusView,
    PinView, ScanStatsView, TransferDirection,
};
pub use transport::{
    create_in_memory_pair, create_in_memory_pair_with_buffer_size, InMemoryConnection,
    InMemoryStream,
};

#[cfg(unix)]
pub use transport::unix::{IpcClient, IpcServer};

#[cfg(windows)]
pub use transport::windows::{IpcClient, IpcServer};
