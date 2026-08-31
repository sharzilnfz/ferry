




pub mod backend;
pub mod client;
pub mod error;
pub mod framing;
pub mod pairing;
pub mod paths;
pub mod protocol;
pub mod transport;

pub use backend::{
    connect_auto, AutoBackend, BoxFuture, InventoryDomain, OpError, PairResult, PinRecord,
    PinReleaseSummary, PinStopSummary, SessionDomain, ShareOffer, ShareStatus, StatusDomain,
    UiBackend, UiEvent, UiEventStream, DAEMON_UNREACHABLE,
};
#[cfg(any(test, debug_assertions, feature = "test-util"))]
pub use backend::FakeBackend;
pub use client::{DaemonClient, ReconnectPolicy};
pub use error::IpcError;



pub use ferry_folder::inventory::{
    default_listing_root, sort_entries, validate_and_normalize, validate_path, DirectoryEntry,
    FolderInventory, FolderRecord, ListDirectoryRequest, ListDirectoryResponse,
};
pub use framing::{IpcConnection, IpcReceiver, IpcSender, DEFAULT_MAX_MESSAGE_SIZE};
pub use pairing::{CreatePairingRequest, CreatePairingResponse, JoinPairingRequest, PairingCode};
#[allow(deprecated)]
pub use paths::socket_path_for_dir;
pub use paths::{default_socket_path, DEFAULT_SOCKET_FILENAME, DEFAULT_WINDOWS_PIPE_PREFIX};
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
