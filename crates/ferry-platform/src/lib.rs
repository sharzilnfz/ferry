pub mod casefold;
pub mod human;
pub mod links;
pub mod lock;
pub mod procs;
pub mod reserved;
pub mod sync_state;
pub mod time;
pub mod winpath;

pub use casefold::{find_case_conflict, fold_key, host_folds_case, CaseConflict, CaseFoldIndex};
pub use human::format_bytes;
pub use links::{allow_windows_dir_links, classify_link, LinkDecision, LinkRefusal};
pub use lock::{
    is_running, read_pid, running_pid, terminate, DaemonLock, DaemonLockError, PidRecord,
    TerminateOutcome, PID_FILENAME, TERMINATE_DEADLINE,
};
pub use procs::{process_start_token, spawn_sleeper};
pub use reserved::is_reserved_device_name;
pub use sync_state::SyncState;
pub use time::{
    civil_utc, current_time_str, fmt_compact, fmt_rfc3339, fmt_time_utc, join_unix, now_unix,
    parse_rfc3339_to_unix, split_unix,
};
pub use winpath::{extend_path, needs_extended_length, MAX_PATH};
