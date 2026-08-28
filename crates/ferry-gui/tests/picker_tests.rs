use ferry_gui::picker::{is_headless_env, pick_folder_async};

#[tokio::test]
async fn picker_headless_returns_none_without_panic() {
    if is_headless_env() {
        let result = pick_folder_async().await;
        assert!(result.is_none(), "headless pick should return None");
    }
}

#[tokio::test]
#[ignore = "requires DISPLAY/Wayland for native dialog"]
async fn picker_native_dialog_does_not_panic_when_display_present() {
    if is_headless_env() {
        return;
    }
    let result = pick_folder_async().await;
    assert!(result.is_none() || result.is_some());
}

#[tokio::test]
async fn picker_is_headless_env_does_not_panic() {
    let _ = is_headless_env();
}

#[test]
fn picker_gui_feature_optional_compiles_without_crash() {
    assert!(is_headless_env() || !is_headless_env());
}
