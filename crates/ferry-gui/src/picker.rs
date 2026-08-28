use std::path::PathBuf;

fn is_headless_inner() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[must_use]
pub fn is_headless_env() -> bool {
    is_headless_inner()
}

#[cfg(feature = "gui")]
pub async fn pick_folder_async() -> Option<PathBuf> {
    if is_headless_inner() {
        return None;
    }
    rfd::AsyncFileDialog::new().pick_folder().await.map(|h| h.path().to_path_buf())
}

#[cfg(not(feature = "gui"))]
#[allow(clippy::unused_async)]
pub async fn pick_folder_async() -> Option<PathBuf> {
    let _ = is_headless_inner();
    None
}
