use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use ferry_folder::inventory::{DirectoryEntry, FolderRecord};

use crate::pairing::{CreatePairingRequest, CreatePairingResponse, JoinPairingRequest};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum DaemonMessage {
    Snapshot(EngineSnapshot),

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

    ConflictRecorded {
        path: String,
        conflict_path: String,
        timestamp: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quarantined_as: Option<String>,
    },

    Ack {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    Pong,

    Error {
        code: String,
        message: String,
    },

    DirectoryListing {
        entries: Vec<DirectoryEntry>,
        absolute_path: PathBuf,
    },

    FolderList {
        folders: Vec<FolderRecord>,
    },

    FolderRegistered {
        folder: FolderRecord,
    },

    FolderRemoved {
        folder_id: String,
    },

    PairingCreated {
        response: CreatePairingResponse,
    },

    PairingJoined {
        result: crate::backend::PairResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum ClientCommand {
    GetStatus,

    StartPin {
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_hours: Option<u64>,
    },

    ReleasePin,

    TriggerScan,

    ListConflicts,

    Ping,

    ListDirectory {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },

    ListFolders,

    RegisterFolder {
        path: PathBuf,
    },

    RemoveFolder {
        folder_id: String,
    },

    CreatePairingSession {
        req: CreatePairingRequest,
    },

    JoinPairingSession {
        req: JoinPairingRequest,
    },
}

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
    #[serde(default)]
    pub discovered_devices: Vec<DiscoveredDeviceView>,
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
            discovered_devices: Vec::new(),
            conflicts: 0,
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredDeviceView {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default)]
    pub status: String,
}

impl DiscoveredDeviceView {
    #[must_use]
    pub fn new(device_id: impl Into<String>, address: Option<String>) -> Self {
        Self {
            device_id: device_id.into(),
            address,
            status: "discovered".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Sending,
    Receiving,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStamp {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_sec: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_nsec: Option<u32>,
}
