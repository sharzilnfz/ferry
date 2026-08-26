//! `ferry-tui`: High-performance terminal dashboard and event-driven monitoring UI for Ferry.
//!
//! Renders real-time synchronization state, storage metrics, peer connectivity, and activity logs
//! over local IPC with zero CPU overhead at idle.

pub mod activity_log;
pub mod app;
pub mod error;
pub mod state;
pub mod terminal;
pub mod timefmt;
pub mod ui;

pub use activity_log::{ActivityLog, LogEntry, LogLevel};
pub use app::TuiApp;
pub use error::TuiError;
pub use state::{format_bytes, SyncState, TransferProgressState, TuiState};
pub use terminal::{install_panic_hook, restore_terminal_writer, TerminalEvents, TerminalGuard};
