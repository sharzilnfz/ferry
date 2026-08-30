//! `ferry-gui`: Pure-Rust Desktop GUI application for Ferry built with `egui` and `eframe`.
//!
//! Provides the Obsidian Dark fluid-glass graphical interface for real-time folder sync
//! monitoring, session pinning, conflict management, and device pairing.

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
pub use app::{format_bytes, BackendAction, GuiApp, GuiTransferState};
pub use beacon::BeaconState;
use eframe::NativeOptions;
use ferry_ipc::backend::{connect_auto, UiBackend};
pub use theme::{colors, Theme};

/// Launch the Ferry native desktop GUI window.
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

/// Launch the Ferry native desktop GUI window with an automated backend connection.
pub fn run_gui_auto(
    socket_path: impl Into<PathBuf>,
    folder_path: impl Into<Option<PathBuf>>,
    rt_handle: tokio::runtime::Handle,
) -> Result<(), eframe::Error> {
    let backend = Arc::new(connect_auto(socket_path, folder_path));
    run_gui(backend, rt_handle)
}
