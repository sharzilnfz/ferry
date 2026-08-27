//! `ferry-gui`: Pure-Rust Desktop GUI application for Ferry built with `egui` and `eframe`.
//!
//! Provides the Obsidian Dark fluid-glass graphical interface for real-time folder sync
//! monitoring, session pinning, conflict management, and device pairing.

pub mod app;
pub mod theme;

use std::sync::Arc;

pub use app::{format_bytes, BackendAction, GuiApp, GuiTransferState};
use eframe::NativeOptions;
use ferry_ipc::backend::UiBackend;
pub use theme::{colors, Theme};

/// Launch the Ferry native desktop GUI window.
pub fn run_gui(
    backend: Arc<dyn UiBackend>,
    rt_handle: tokio::runtime::Handle,
) -> Result<(), eframe::Error> {
    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ferry")
            .with_inner_size([720.0, 560.0])
            .with_min_inner_size([500.0, 400.0])
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
