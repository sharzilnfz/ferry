use std::path::{Path, PathBuf};

use ferry_ipc::{
    default_socket_path, socket_path_for_dir, socket_path_for_folder_id, DEFAULT_SOCKET_FILENAME,
};

#[test]
fn test_default_socket_path() {
    let path = default_socket_path();
    #[cfg(unix)]
    {
        assert!(path.ends_with(DEFAULT_SOCKET_FILENAME));
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
        assert_eq!(path, PathBuf::from("/custom/project/.ferry/daemon.sock"));
    }
    #[cfg(windows)]
    {
        assert!(path.to_string_lossy().starts_with(r"\\.\pipe\ferry-"));
    }

    // When dir already ends in .ferry
    let ferry_dir = Path::new("/custom/project/.ferry");
    let path2 = socket_path_for_dir(ferry_dir);
    #[cfg(unix)]
    {
        assert_eq!(path2, PathBuf::from("/custom/project/.ferry/daemon.sock"));
    }
}

#[test]
fn test_socket_path_for_folder_id() {
    let folder_id = "0123456789abcdef0123456789abcdef";
    let path = socket_path_for_folder_id(folder_id);

    #[cfg(unix)]
    {
        assert!(path.ends_with(DEFAULT_SOCKET_FILENAME));
    }
    #[cfg(windows)]
    {
        assert_eq!(
            path,
            PathBuf::from(format!(r"\\.\pipe\ferry-{}", folder_id))
        );
    }
}
