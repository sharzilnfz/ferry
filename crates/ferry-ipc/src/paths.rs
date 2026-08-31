

use std::path::{Path, PathBuf};


pub const DEFAULT_SOCKET_FILENAME: &str = "daemon.sock";


pub const DEFAULT_WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\ferry";

fn ferry_home_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    PathBuf::from("/tmp/.ferry")
}




pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"{DEFAULT_WINDOWS_PIPE_PREFIX}-daemon"))
    }
    #[cfg(not(windows))]
    {
        ferry_home_dir().join(DEFAULT_SOCKET_FILENAME)
    }
}







#[deprecated(
    since = "0.1.0",
    note = "use default_socket_path() instead; socket is now device-global"
)]
pub fn socket_path_for_dir(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        
        let root = if dir.file_name().and_then(|n| n.to_str()) == Some(".ferry") {
            dir.parent().unwrap_or(dir)
        } else {
            dir
        };

        let abs = std::fs::canonicalize(root).unwrap_or_else(|_| {
            if root.is_relative() {
                std::env::current_dir().map_or_else(|_| root.to_path_buf(), |cwd| cwd.join(root))
            } else {
                root.to_path_buf()
            }
        });

        let mut norm = abs.to_string_lossy().replace('/', "\\").to_lowercase();
        if let Some(stripped) = norm.strip_prefix(r"\\?\") {
            norm = stripped.to_string();
        }

        let mut hasher = DefaultHasher::new();
        norm.hash(&mut hasher);
        let h = hasher.finish();
        PathBuf::from(format!(r"{DEFAULT_WINDOWS_PIPE_PREFIX}-{h:016x}"))
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
