//! Fixed circular buffer for activity log entries in the Ferry TUI.

use std::collections::VecDeque;
use ferry_ipc::protocol::DaemonMessage;

/// Severity level for log items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

/// A single activity log record with timestamp, content, and severity level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub timestamp: String,
    pub message: String,
    pub level: LogLevel,
}

impl LogEntry {
    #[must_use]
    pub fn new(timestamp: impl Into<String>, message: impl Into<String>, level: LogLevel) -> Self {
        Self {
            timestamp: timestamp.into(),
            message: message.into(),
            level,
        }
    }
}

/// Fixed circular buffer storing up to `capacity` log entries.
#[derive(Debug, Clone)]
pub struct ActivityLog {
    entries: VecDeque<LogEntry>,
    capacity: usize,
}

impl Default for ActivityLog {
    fn default() -> Self {
        Self::new(100)
    }
}

impl ActivityLog {
    /// Create a new activity log bounded by `capacity` entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            entries: VecDeque::with_capacity(cap),
            capacity: cap,
        }
    }

    /// Append a new log entry, discarding the oldest if at capacity.
    pub fn push(&mut self, entry: LogEntry) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Push an informational log message.
    pub fn push_info(&mut self, timestamp: impl Into<String>, message: impl Into<String>) {
        self.push(LogEntry::new(timestamp, message, LogLevel::Info));
    }

    /// Push a warning log message.
    pub fn push_warn(&mut self, timestamp: impl Into<String>, message: impl Into<String>) {
        self.push(LogEntry::new(timestamp, message, LogLevel::Warn));
    }

    /// Push an error log message.
    pub fn push_error(&mut self, timestamp: impl Into<String>, message: impl Into<String>) {
        self.push(LogEntry::new(timestamp, message, LogLevel::Error));
    }

    /// Push a success log message.
    pub fn push_success(&mut self, timestamp: impl Into<String>, message: impl Into<String>) {
        self.push(LogEntry::new(timestamp, message, LogLevel::Success));
    }

    /// Record a high-level summary of an incoming `DaemonMessage`.
    pub fn record_daemon_message(&mut self, timestamp: impl Into<String>, msg: &DaemonMessage) {
        let ts = timestamp.into();
        match msg {
            DaemonMessage::Snapshot(s) => {
                self.push_info(
                    ts,
                    format!("Loaded state snapshot (state: {}, folder: {})", s.state, s.folder),
                );
            }
            DaemonMessage::StateChanged {
                state,
                manifest_id,
                ..
            } => {
                let short_id = if manifest_id.len() > 12 {
                    &manifest_id[..12]
                } else {
                    manifest_id
                };
                self.push_info(
                    ts,
                    format!("Engine state changed to {state} (manifest: {short_id})"),
                );
            }
            DaemonMessage::TransferProgress {
                bytes_transferred,
                total_bytes,
                current_path,
                ..
            } => {
                if *total_bytes > 0 && *bytes_transferred >= *total_bytes {
                    self.push_success(ts, format!("Transfer complete: {current_path}"));
                }
            }
            DaemonMessage::ConflictRecorded {
                path,
                conflict_path,
                ..
            } => {
                self.push_warn(
                    ts,
                    format!("Conflict recorded: {path} quarantined as {conflict_path}"),
                );
            }
            DaemonMessage::Ack { command, message } => {
                let detail = message.as_deref().unwrap_or("ok");
                self.push_info(ts, format!("Command '{command}' acknowledged: {detail}"));
            }
            DaemonMessage::Pong => {
                self.push_info(ts, "Daemon heartbeat pong received");
            }
            DaemonMessage::Error { code, message } => {
                self.push_error(ts, format!("Daemon error [{code}]: {message}"));
            }
        }
    }

    /// Read-only slice/queue of the stored log entries in chronological order.
    #[must_use]
    pub fn entries(&self) -> &VecDeque<LogEntry> {
        &self.entries
    }

    /// Number of entries currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no log entries are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Maximum capacity of this circular buffer.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clear all log entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
