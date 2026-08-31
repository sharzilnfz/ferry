use crate::activity_log::ActivityLog;
use ferry_ipc::protocol::{
    ConflictEntry, DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView,
    TransferDirection,
};
pub use ferry_platform::format_bytes;
use ferry_platform::time::current_time_str;
pub use ferry_platform::SyncState;
use std::collections::HashMap;

pub trait SyncStateBadge {
    fn badge_color(&self) -> ratatui::style::Color;
}

impl SyncStateBadge for SyncState {
    fn badge_color(&self) -> ratatui::style::Color {
        match self {
            Self::Synced => ratatui::style::Color::Green,
            Self::Syncing => ratatui::style::Color::Cyan,
            Self::Conflict => ratatui::style::Color::Red,
            Self::Pinned => ratatui::style::Color::Magenta,
            Self::Idle => ratatui::style::Color::Gray,
            Self::Error => ratatui::style::Color::Red,
            Self::Offline => ratatui::style::Color::DarkGray,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgressState {
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub current_path: String,
    pub chunks_transferred: Option<u64>,
    pub total_chunks: Option<u64>,
    pub peer_device_id: Option<String>,
    pub direction: Option<TransferDirection>,
}

#[derive(Debug, Clone)]
pub struct TuiState {
    pub folder: String,
    pub folder_id: String,
    pub device_id: String,
    pub manifest_id: String,
    pub engine_state: SyncState,
    pub raw_state_str: String,
    pub scanned: ScanStatsView,
    pub pending_changes: Option<i64>,
    pub pin: PinView,
    pub held_changes: usize,
    pub held_by_peer: HashMap<String, Vec<String>>,
    pub peers: Vec<PeerStatusView>,
    pub conflicts: usize,
    pub active_transfer: Option<TransferProgressState>,
    pub activity_log: ActivityLog,
    pub show_conflicts_modal: bool,
    pub conflict_entries: Vec<ConflictEntry>,
    pub is_connected: bool,
    pub should_quit: bool,

    pub cached_metrics_line: String,
    pub cached_manifest_line: String,
    pub cached_pin_line: String,
    pub cached_progress_label: String,
    pub cached_progress_percent: u16,
    pub cached_progress_ratio: f64,
}

impl Default for TuiState {
    fn default() -> Self {
        let mut s = Self {
            folder: "-".to_string(),
            folder_id: "-".to_string(),
            device_id: "-".to_string(),
            manifest_id: String::new(),
            engine_state: SyncState::Offline,
            raw_state_str: "offline".to_string(),
            scanned: ScanStatsView::default(),
            pending_changes: None,
            pin: PinView::none(),
            held_changes: 0,
            held_by_peer: HashMap::new(),
            peers: Vec::new(),
            conflicts: 0,
            active_transfer: None,
            activity_log: ActivityLog::default(),
            show_conflicts_modal: false,
            conflict_entries: Vec::new(),
            is_connected: false,
            should_quit: false,
            cached_metrics_line: String::new(),
            cached_manifest_line: String::new(),
            cached_pin_line: String::new(),
            cached_progress_label: "Idle (no active transfer)".to_string(),
            cached_progress_percent: 0,
            cached_progress_ratio: 0.0,
        };
        s.update_cached_strings();
        s
    }
}

impl TuiState {
    #[must_use]
    pub fn new(
        folder: impl Into<String>,
        folder_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        let mut s = Self {
            folder: folder.into(),
            folder_id: folder_id.into(),
            device_id: device_id.into(),
            ..Default::default()
        };
        s.update_cached_strings();
        s
    }

    #[must_use]
    pub fn resolve_sync_state(&self) -> SyncState {
        if !self.is_connected && self.raw_state_str.eq_ignore_ascii_case("offline") {
            SyncState::Offline
        } else if self.pin.holding
            || self.pin.state.eq_ignore_ascii_case("active")
            || self.raw_state_str.eq_ignore_ascii_case("pinned")
        {
            SyncState::Pinned
        } else if self.conflicts > 0 || self.raw_state_str.eq_ignore_ascii_case("conflict") {
            SyncState::Conflict
        } else if self.raw_state_str.eq_ignore_ascii_case("syncing")
            || self.active_transfer.is_some()
        {
            SyncState::Syncing
        } else if self.raw_state_str.eq_ignore_ascii_case("synced") {
            SyncState::Synced
        } else if self.raw_state_str.eq_ignore_ascii_case("idle") {
            SyncState::Idle
        } else if self.raw_state_str.eq_ignore_ascii_case("error") {
            SyncState::Error
        } else if self.raw_state_str.eq_ignore_ascii_case("offline") {
            SyncState::Offline
        } else {
            SyncState::Synced
        }
    }

    pub fn apply_snapshot(&mut self, snapshot: EngineSnapshot) {
        self.folder = snapshot.folder;
        self.folder_id = snapshot.folder_id;
        self.device_id = snapshot.device_id;
        self.manifest_id = snapshot.manifest_id.unwrap_or_default();
        self.raw_state_str = snapshot.state;
        self.scanned = snapshot.scanned;
        self.pending_changes = snapshot.pending_changes;
        self.pin = snapshot.pin;
        self.held_changes = snapshot.held_changes;
        self.held_by_peer = snapshot.held_by_peer;
        self.peers = snapshot.peers;
        self.conflicts = snapshot.conflicts;
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log.record_daemon_message(
            current_time_str(),
            &DaemonMessage::Snapshot(EngineSnapshot {
                folder: self.folder.clone(),
                folder_id: self.folder_id.clone(),
                device_id: self.device_id.clone(),
                manifest_id: if self.manifest_id.is_empty() {
                    None
                } else {
                    Some(self.manifest_id.clone())
                },
                state: self.raw_state_str.clone(),
                scanned: self.scanned,
                pending_changes: self.pending_changes,
                pin: self.pin.clone(),
                held_changes: self.held_changes,
                held_by_peer: self.held_by_peer.clone(),
                peers: self.peers.clone(),
                conflicts: self.conflicts,
            }),
        );
    }

    pub fn apply_state_changed(
        &mut self,
        state: String,
        manifest_id: String,
        agreed_id: Option<String>,
        pending_changes: Option<i64>,
        stats: Option<ScanStatsView>,
    ) {
        self.raw_state_str.clone_from(&state);
        self.manifest_id.clone_from(&manifest_id);
        if let Some(pending) = pending_changes {
            self.pending_changes = Some(pending);
        }
        if let Some(scanned) = stats {
            self.scanned = scanned;
        }
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log.record_daemon_message(
            current_time_str(),
            &DaemonMessage::StateChanged {
                state,
                manifest_id,
                agreed_id,
                pending_changes,
                stats,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_transfer_progress(
        &mut self,
        bytes_transferred: u64,
        total_bytes: u64,
        current_path: String,
        chunks_transferred: Option<u64>,
        total_chunks: Option<u64>,
        peer_device_id: Option<String>,
        direction: Option<TransferDirection>,
    ) {
        let is_done = total_bytes > 0 && bytes_transferred >= total_bytes;
        self.active_transfer = if is_done {
            None
        } else {
            Some(TransferProgressState {
                bytes_transferred,
                total_bytes,
                current_path: current_path.clone(),
                chunks_transferred,
                total_chunks,
                peer_device_id: peer_device_id.clone(),
                direction,
            })
        };
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log.record_daemon_message(
            current_time_str(),
            &DaemonMessage::TransferProgress {
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            },
        );
    }

    pub fn apply_conflict_recorded(
        &mut self,
        path: String,
        conflict_path: String,
        timestamp: u64,
        quarantined_as: Option<String>,
    ) {
        self.conflicts += 1;
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log.record_daemon_message(
            current_time_str(),
            &DaemonMessage::ConflictRecorded {
                path,
                conflict_path,
                timestamp,
                quarantined_as,
            },
        );
    }

    pub fn apply_error(&mut self, code: String, message: String) {
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Error { code, message });
    }

    pub fn apply_ack(&mut self, command: String, message: Option<String>) {
        self.is_connected = true;
        if command == "list_conflicts" {
            if let Some(ref msg_json) = message {
                if let Ok(entries) = serde_json::from_str::<Vec<ConflictEntry>>(msg_json) {
                    self.conflict_entries = entries;
                    self.conflicts = self.conflict_entries.len().max(self.conflicts);
                }
            }
        }
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Ack { command, message });
    }

    pub fn apply_pong(&mut self) {
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Pong);
    }

    pub fn handle_daemon_message(&mut self, msg: DaemonMessage) {
        match msg {
            DaemonMessage::Snapshot(s) => self.apply_snapshot(s),
            DaemonMessage::StateChanged {
                state,
                manifest_id,
                agreed_id,
                pending_changes,
                stats,
            } => self.apply_state_changed(state, manifest_id, agreed_id, pending_changes, stats),
            DaemonMessage::TransferProgress {
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            } => self.apply_transfer_progress(
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            ),
            DaemonMessage::ConflictRecorded {
                path,
                conflict_path,
                timestamp,
                quarantined_as,
            } => self.apply_conflict_recorded(path, conflict_path, timestamp, quarantined_as),
            DaemonMessage::Ack { command, message } => self.apply_ack(command, message),
            DaemonMessage::Pong => self.apply_pong(),
            DaemonMessage::Error { code, message } => self.apply_error(code, message),
            _ => {
                self.is_connected = true;
                self.engine_state = self.resolve_sync_state();
                self.update_cached_strings();
            }
        }
    }

    pub fn update_cached_strings(&mut self) {
        self.cached_metrics_line = format!(
            "{} files, {} dirs, {} symlinks ({})",
            self.scanned.files,
            self.scanned.dirs,
            self.scanned.symlinks,
            format_bytes(self.scanned.bytes_chunked),
        );
        self.cached_manifest_line = if self.manifest_id.is_empty() {
            "none".to_string()
        } else {
            self.manifest_id.clone()
        };
        self.cached_pin_line = if self.pin.holding {
            if self.pin.paths.is_empty() {
                "active (entire folder)".to_string()
            } else {
                format!("active ({} paths)", self.pin.paths.len())
            }
        } else {
            self.pin.state.clone()
        };

        if let Some(ref transfer) = self.active_transfer {
            let total = transfer.total_bytes.max(1);
            let ratio = (transfer.bytes_transferred as f64 / total as f64).clamp(0.0, 1.0);
            let percent = (ratio * 100.0) as u16;
            let dir_str = match transfer.direction {
                Some(TransferDirection::Sending) => "Sending",
                Some(TransferDirection::Receiving) => "Receiving",
                None => "Syncing",
            };
            let chunks_info = match (transfer.chunks_transferred, transfer.total_chunks) {
                (Some(c), Some(t)) => format!(" [{c}/{t} chunks]"),
                _ => String::new(),
            };
            self.cached_progress_ratio = ratio;
            self.cached_progress_percent = percent;
            self.cached_progress_label = format!(
                "{} {}%: {} / {} ({}){}",
                dir_str,
                percent,
                format_bytes(transfer.bytes_transferred),
                format_bytes(transfer.total_bytes),
                transfer.current_path,
                chunks_info,
            );
        } else {
            self.cached_progress_ratio = 0.0;
            self.cached_progress_percent = 0;
            self.cached_progress_label = "Idle (no active transfer)".to_string();
        }
    }
}
