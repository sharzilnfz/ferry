#![allow(deprecated)]

use ferry_ipc::{default_socket_path, socket_path_for_dir};
use std::path::Path;

#[test]
fn test_default_socket_path() {
    let path = default_socket_path();
    #[cfg(unix)]
    {
        assert!(path.ends_with(ferry_ipc::DEFAULT_SOCKET_FILENAME));
        assert!(path.to_string_lossy().contains(".ferry"));
    }
    #[cfg(windows)]
    {
        assert!(path.to_string_lossy().starts_with(r"\\.\pipe\ferry"));
    }
}

#[test]
fn test_socket_path_for_dir() {
    let base_dir = Path::new("/custom/project");
    let path = socket_path_for_dir(base_dir);

    #[cfg(unix)]
    {
        assert_eq!(
            path,
            std::path::PathBuf::from("/custom/project/.ferry/daemon.sock")
        );
    }
    #[cfg(windows)]
    {
        assert!(path.to_string_lossy().starts_with(r"\\.\pipe\ferry-"));
    }

    let ferry_dir = Path::new("/custom/project/.ferry");
    let path2 = socket_path_for_dir(ferry_dir);
    #[cfg(unix)]
    {
        assert_eq!(
            path2,
            std::path::PathBuf::from("/custom/project/.ferry/daemon.sock")
        );
    }
    #[cfg(windows)]
    {
        assert!(path2.to_string_lossy().starts_with(r"\\.\pipe\ferry-"));
    }
}
