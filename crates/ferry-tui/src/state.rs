//! UI State model and transitions for Ferry TUI.

use crate::activity_log::ActivityLog;
use ferry_ipc::backend::{DirectoryListing, FsEntry};
use ferry_ipc::protocol::{
    ConflictEntry, DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView,
    TransferDirection,
};
use ferry_platform::time::current_time_str;
use std::collections::HashMap;
use std::path::PathBuf;

/// Core synchronization state badge displayed in the TUI header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Offline,
    Synced,
    Syncing,
    Conflict,
    Pinned,
    Idle,
    Error,
}

impl SyncState {
    /// Text label displayed on the engine state badge.
    #[must_use]
    pub const fn badge_text(&self) -> &'static str {
        match self {
            Self::Synced => "SYNCED",
            Self::Syncing => "SYNCING",
            Self::Conflict => "CONFLICT",
            Self::Pinned => "PINNED",
            Self::Idle => "IDLE",
            Self::Error => "ERROR",
            Self::Offline => "OFFLINE",
        }
    }

    /// Primary badge foreground color.
    #[must_use]
    pub const fn badge_color(&self) -> ratatui::style::Color {
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

/// Active chunk or file transfer state.
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

/// Format bytes into human-readable representation.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    }
}

/// Single entry displayed in the filesystem explorer modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderPickerItem {
    Parent(PathBuf),
    Entry(FsEntry),
}

/// Interactive filesystem explorer modal state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderPickerState {
    pub current_path: PathBuf,
    pub parent_path: Option<PathBuf>,
    pub raw_entries: Vec<FsEntry>,
    pub filter_query: String,
    pub selected_index: usize,
    pub error_message: Option<String>,
}

impl FolderPickerState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset query and populate listing from backend.
    pub fn set_listing(&mut self, listing: DirectoryListing) {
        self.current_path = listing.current_path;
        self.parent_path = listing.parent_path;
        self.raw_entries = listing.entries;
        self.error_message = None;
        self.clamp_selection();
    }

    /// Return all items matching the active filter query.
    #[must_use]
    pub fn filtered_items(&self) -> Vec<FolderPickerItem> {
        let mut items = Vec::new();
        let query = self.filter_query.to_lowercase();

        if let Some(ref parent) = self.parent_path {
            if query.is_empty() || "..".contains(&query) {
                items.push(FolderPickerItem::Parent(parent.clone()));
            }
        }

        for entry in &self.raw_entries {
            if query.is_empty() || entry.name.to_lowercase().contains(&query) {
                items.push(FolderPickerItem::Entry(entry.clone()));
            }
        }

        items
    }

    /// Move the active list highlight up.
    pub fn move_selection_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move the active list highlight down.
    pub fn move_selection_down(&mut self) {
        let total = self.filtered_items().len();
        if total > 0 && self.selected_index + 1 < total {
            self.selected_index += 1;
        }
    }

    /// Append a typed character to the live filter query.
    pub fn append_filter(&mut self, c: char) {
        self.filter_query.push(c);
        self.selected_index = 0;
    }

    /// Remove the last character from the live filter query.
    pub fn backspace_filter(&mut self) {
        self.filter_query.pop();
        self.selected_index = 0;
    }

    /// Clear the live filter query.
    pub fn clear_filter(&mut self) {
        self.filter_query.clear();
        self.selected_index = 0;
    }

    /// Ensure selected index is within bounds of filtered items.
    pub fn clamp_selection(&mut self) {
        let total = self.filtered_items().len();
        if total == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= total {
            self.selected_index = total - 1;
        }
    }

    /// Currently highlighted item in the filtered list.
    #[must_use]
    pub fn selected_item(&self) -> Option<FolderPickerItem> {
        let items = self.filtered_items();
        items.get(self.selected_index).cloned()
    }

    /// Path corresponding to the highlighted entry or current working directory.
    #[must_use]
    pub fn highlighted_path(&self) -> PathBuf {
        match self.selected_item() {
            Some(FolderPickerItem::Parent(p)) => p,
            Some(FolderPickerItem::Entry(e)) => e.path,
            None => self.current_path.clone(),
        }
    }
}

/// Complete in-memory state of the Ferry TUI dashboard.
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
    pub show_folder_picker_modal: bool,
    pub folder_picker: FolderPickerState,
    pub is_connected: bool,
    pub should_quit: bool,

    // Pre-computed strings for zero-allocation rendering in terminal draw loop:
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
            show_folder_picker_modal: false,
            folder_picker: FolderPickerState::default(),
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
    /// Create a new state initialized with folder and device identifiers.
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

    /// Resolve the authoritative high-level sync state from current fields.
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

    /// Apply an `EngineSnapshot` received from the daemon.
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

    /// Apply an engine state change notification.
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

    /// Apply an active transfer progress notification.
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

    /// Apply a recorded conflict notification.
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

    /// Apply an error notification from the daemon.
    pub fn apply_error(&mut self, code: String, message: String) {
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Error { code, message });
    }

    /// Apply an acknowledgement notification.
    pub fn apply_ack(&mut self, command: String, message: Option<String>) {
        self.is_connected = true;
        if command == "list_conflicts" {
            if let Some(ref msg_json) = message {
                if let Ok(entries) = serde_json::from_str::<Vec<ConflictEntry>>(msg_json) {
                    self.conflict_entries = entries;
                    self.conflicts = self.conflict_entries.len().max(self.conflicts);
                }
            }
        } else if command == "list_directory" {
            if let Some(ref msg_json) = message {
                if let Ok(listing) = serde_json::from_str::<DirectoryListing>(msg_json) {
                    self.folder_picker.set_listing(listing);
                }
            }
        } else if command == "register_folder" {
            if let Some(ref path_str) = message {
                self.folder.clone_from(path_str);
                self.show_folder_picker_modal = false;
            }
        }
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Ack { command, message });
    }

    /// Apply a heartbeat pong notification.
    pub fn apply_pong(&mut self) {
        self.is_connected = true;
        self.engine_state = self.resolve_sync_state();
        self.update_cached_strings();
        self.activity_log
            .record_daemon_message(current_time_str(), &DaemonMessage::Pong);
    }

    /// Process any generic `DaemonMessage`.
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
        }
    }

    /// Update pre-computed strings for zero-allocation rendering inside ratatui's draw loop.
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
