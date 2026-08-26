//! Platform-specific socket and named pipe path resolution.

use std::path::{Path, PathBuf};

/// Default socket filename under `.ferry`.
pub const DEFAULT_SOCKET_FILENAME: &str = "daemon.sock";

/// Default Windows named pipe prefix.
pub const DEFAULT_WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\ferry";

/// Returns the default global daemon socket path:
/// - Unix: `~/.ferry/daemon.sock` (or `/tmp/.ferry/daemon.sock` if `$HOME` is unset)
/// - Windows: `\\.\pipe\ferry-daemon`
pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"{}-daemon", DEFAULT_WINDOWS_PIPE_PREFIX))
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".ferry").join(DEFAULT_SOCKET_FILENAME)
        } else {
            PathBuf::from("/tmp/.ferry").join(DEFAULT_SOCKET_FILENAME)
        }
    }
}

/// Returns the socket path for a specific folder or store directory:
/// - Unix: `<dir>/daemon.sock` if `dir` ends in `.ferry`, otherwise `<dir>/.ferry/daemon.sock`
/// - Windows: `\\.\pipe\ferry-<path_hash>`
pub fn socket_path_for_dir(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let canonical = dir.to_string_lossy();
        let mut hasher = DefaultHasher::new();
        canonical.hash(&mut hasher);
        let h = hasher.finish();
        PathBuf::from(format!(r"{}-{:016x}", DEFAULT_WINDOWS_PIPE_PREFIX, h))
    }
    #[cfg(not(windows))]
    {
        if dir.file_name().and_then(|n| n.to_str()) == Some(".ferry") {
            dir.join(DEFAULT_SOCKET_FILENAME)
        } else {
            dir.join(".ferry").join(DEFAULT_SOCKET_FILENAME)
        }
    }
}
