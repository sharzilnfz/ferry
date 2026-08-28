//! Typed IPC messages and wire protocol data structures for Ferry.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Server push messages emitted by the sync daemon over IPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Full snapshot of current daemon and folder state.
    Snapshot(EngineSnapshot),

    /// Engine state transition or update.
    StateChanged {
        state: String,
        manifest_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agreed_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_changes: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<ScanStatsView>,
    },

    /// Progress report for active chunk or blob transfer.
    TransferProgress {
        bytes_transferred: u64,
        total_bytes: u64,
        current_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunks_transferred: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_chunks: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<TransferDirection>,
    },

    /// Notification that a new file conflict has been detected and quarantined.
    ConflictRecorded {
        path: String,
        conflict_path: String,
        timestamp: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quarantined_as: Option<String>,
    },

    /// General acknowledgement response for a client command.
    Ack {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Heartbeat response.
    Pong,

    /// Error notification or command failure response.
    Error { code: String, message: String },
}

/// Commands sent from a client (CLI / TUI / Web proxy) to the sync daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum ClientCommand {
    /// Request an immediate `DaemonMessage::Snapshot`.
    GetStatus,

    /// Start a session pin on the specified paths (empty paths means entire folder).
    StartPin {
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_hours: Option<u64>,
    },

    /// Release any active session pin and reconcile held changes.
    ReleasePin,

    /// Trigger an immediate manual filesystem rescan.
    TriggerScan,

    /// List recorded conflicts.
    ListConflicts,

    /// List entries in a directory for interactive file pickers.
    ListDirectory {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// List all configured folders managed by the device daemon.
    ListFolders,

    /// Register and start synchronizing a new folder.
    RegisterFolder {
        path: String,
    },

    /// Unregister a folder from the device daemon.
    UnregisterFolder {
        folder_id: String,
    },

    /// Switch active folder context for status reporting.
    SwitchFolder {
        folder_id: String,
    },

    /// Create an in-band pairing session with a short ephemeral code.
    CreatePairingSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_id: Option<String>,
    },

    /// Join an in-band pairing session using a 6-word or short code.
    JoinPairingSession {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_dir: Option<String>,
    },

    /// Ping request to test connectivity and liveness.
    Ping,
}

/// Snapshot of the complete engine state, matching the CLI `--json` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineSnapshot {
    pub folder: String,
    pub folder_id: String,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_id: Option<String>,
    pub state: String,
    pub scanned: ScanStatsView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_changes: Option<i64>,
    pub pin: PinView,
    pub held_changes: usize,
    #[serde(default)]
    pub held_by_peer: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub peers: Vec<PeerStatusView>,
    pub conflicts: usize,
}

impl EngineSnapshot {
    #[must_use]
    pub fn new(
        folder: impl Into<String>,
        folder_id: impl Into<String>,
        device_id: impl Into<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            folder: folder.into(),
            folder_id: folder_id.into(),
            device_id: device_id.into(),
            manifest_id: None,
            state: state.into(),
            scanned: ScanStatsView::default(),
            pending_changes: None,
            pin: PinView::default(),
            held_changes: 0,
            held_by_peer: HashMap::new(),
            peers: Vec::new(),
            conflicts: 0,
        }
    }
}

/// Filesystem scan statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScanStatsView {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes_chunked: u64,
}

impl ScanStatsView {
    #[must_use]
    pub const fn new(files: u64, dirs: u64, symlinks: u64, bytes_chunked: u64) -> Self {
        Self {
            files,
            dirs,
            symlinks,
            bytes_chunked,
        }
    }
}

/// Session pin view.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PinView {
    pub state: String,
    pub holding: bool,
    #[serde(default)]
    pub paths: Vec<String>,
}

impl PinView {
    #[must_use]
    pub fn none() -> Self {
        Self {
            state: "none".to_string(),
            holding: false,
            paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn active(paths: Vec<String>) -> Self {
        Self {
            state: "active".to_string(),
            holding: true,
            paths,
        }
    }
}

/// Connected/paired peer status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatusView {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agreed_manifest_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreed_at: Option<String>,
    pub connectivity: String,
}

impl PeerStatusView {
    #[must_use]
    pub fn new(device_id: impl Into<String>, connectivity: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            last_agreed_manifest_id: None,
            agreed_at: None,
            connectivity: connectivity.into(),
        }
    }
}

/// Direction of transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Sending,
    Receiving,
}

/// One resolved conflict record matching `.ferry/conflicts.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEntry {
    pub ts: String,
    pub folder_id: String,
    pub path: String,
    pub kind: String,
    pub winner: DeviceStamp,
    pub loser: DeviceStamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantined_as: Option<String>,
}

/// Device timestamp stamp in conflict records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStamp {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_sec: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_nsec: Option<u32>,
}
