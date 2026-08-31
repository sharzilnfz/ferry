




pub mod activity;
pub mod app;
pub mod beacon;
pub mod fleet;
pub mod modals;
pub mod picker;
pub mod telemetry;
pub mod theme;

use std::path::PathBuf;
use std::sync::Arc;

pub use activity::ActivityEntry;
pub use app::{BackendAction, GuiApp, GuiTransferState};
pub use beacon::{beacon_color, beacon_label, render_pulsating_beacon, status_beacon_ui};
pub use ferry_platform::format_bytes;
pub use ferry_platform::SyncState;
pub use ferry_platform::SyncState as BeaconState;
use eframe::NativeOptions;
use ferry_ipc::backend::{connect_auto, UiBackend};
pub use theme::{colors, Theme};


pub fn run_gui(
    backend: Arc<dyn UiBackend>,
    rt_handle: tokio::runtime::Handle,
) -> Result<(), eframe::Error> {
    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ferry")
            .with_inner_size([760.0, 600.0])
            .with_min_inner_size([540.0, 440.0])
            .with_transparent(false),
        ..Default::default()
    };

    eframe::run_native(
        "Ferry",
        options,
        Box::new(move |cc| {
            Theme::apply(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(
                backend,
                cc.egui_ctx.clone(),
                rt_handle,
            )))
        }),
    )
}


pub fn run_gui_auto(
    socket_path: impl Into<PathBuf>,
    folder_path: impl Into<Option<PathBuf>>,
    rt_handle: tokio::runtime::Handle,
) -> Result<(), eframe::Error> {
    let backend = Arc::new(connect_auto(socket_path, folder_path));
    run_gui(backend, rt_handle)
}
