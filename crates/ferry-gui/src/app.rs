//! Main `GuiApp` implementation for Ferry Desktop.

use std::sync::Arc;
use std::time::Instant;

use eframe::App;
use egui::{
    epaint::Shadow, vec2, Align, Align2, Area, CentralPanel, Color32, Frame, Key, Layout, Margin,
    Order, RichText, Rounding, ScrollArea, Stroke, TopBottomPanel,
};
use ferry_ipc::backend::{PinRecord, ShareOffer, ShareStatus, UiBackend, UiEvent};
use ferry_ipc::protocol::{ConflictEntry, EngineSnapshot, TransferDirection};

use crate::theme::{colors, Theme};

/// User actions sent asynchronously to the backend worker task.
pub enum BackendAction {
    TriggerScan,
    StartPin { paths: Vec<String>, hours: Option<u64> },
    StopPin,
    ReleasePin,
    InitiateShare { i_know: bool },
    AcceptPair { payload_path: std::path::PathBuf },
    FetchStatus,
    FetchConflicts,
}

/// Active chunk or file transfer state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiTransferState {
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub current_path: String,
    pub chunks_transferred: Option<u64>,
    pub total_chunks: Option<u64>,
    pub peer_device_id: Option<String>,
    pub direction: Option<TransferDirection>,
}

/// Format bytes into human-readable representation.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    }
}

/// The Ferry Desktop GUI Application struct implementing `eframe::App`.
pub struct GuiApp {
    pub backend: Arc<dyn UiBackend>,
    pub snapshot: Option<EngineSnapshot>,
    pub conflicts: Vec<ConflictEntry>,
    pub active_pin: Option<PinRecord>,
    pub active_share: Option<ShareOffer>,
    pub share_status: Option<ShareStatus>,
    pub active_transfer: Option<GuiTransferState>,
    pub activity_log: Vec<(String, String, Color32)>,

    pub show_conflicts_modal: bool,
    pub show_pin_modal: bool,
    pub show_share_modal: bool,
    pub show_pair_modal: bool,

    pub pin_paths_input: String,
    pub pair_path_input: String,
    pub status_message: Option<(String, Instant, Color32)>,

    pub is_connected: bool,
    pub should_quit: bool,

    pub event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<UiEvent>>,
    pub action_tx: Option<tokio::sync::mpsc::UnboundedSender<BackendAction>>,
}

impl GuiApp {
    /// Construct a headless instance for testing and headless verification.
    #[must_use]
    pub fn new_headless(backend: Arc<dyn UiBackend>) -> Self {
        Self {
            backend,
            snapshot: None,
            conflicts: Vec::new(),
            active_pin: None,
            active_share: None,
            share_status: None,
            active_transfer: None,
            activity_log: Vec::new(),
            show_conflicts_modal: false,
            show_pin_modal: false,
            show_share_modal: false,
            show_pair_modal: false,
            pin_paths_input: String::new(),
            pair_path_input: String::new(),
            status_message: None,
            is_connected: false,
            should_quit: false,
            event_rx: None,
            action_tx: None,
        }
    }

    /// Construct a fully wired `GuiApp` with asynchronous worker tasks.
    #[must_use]
    pub fn new(
        backend: Arc<dyn UiBackend>,
        ctx: egui::Context,
        rt_handle: tokio::runtime::Handle,
    ) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (action_tx, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<BackendAction>();

        // 1. Initial snapshot & conflict queries
        let b_clone = backend.clone();
        let ev_tx_clone = event_tx.clone();
        let ctx_clone = ctx.clone();
        rt_handle.spawn(async move {
            if let Ok(snap) = b_clone.get_status().await {
                let _ = ev_tx_clone.send(UiEvent::State(snap));
                ctx_clone.request_repaint();
            }
            if let Ok(confs) = b_clone.list_conflicts().await {
                for c in confs {
                    let _ = ev_tx_clone.send(UiEvent::ConflictRecorded {
                        path: c.path,
                        conflict_path: c.quarantined_as.unwrap_or_default(),
                        timestamp: 0,
                        quarantined_as: None,
                    });
                }
                ctx_clone.request_repaint();
            }
        });

        // 2. Real-time push event listener
        let b_events = backend.clone();
        let ev_tx_stream = event_tx.clone();
        let ctx_stream = ctx.clone();
        rt_handle.spawn(async move {
            if let Ok(mut stream) = b_events.subscribe_events().await {
                while let Ok(event) = stream.recv().await {
                    if ev_tx_stream.send(event).is_err() {
                        break;
                    }
                    ctx_stream.request_repaint();
                }
            }
        });

        // 3. User action processor
        let b_actions = backend.clone();
        let ev_tx_actions = event_tx.clone();
        let ctx_actions = ctx.clone();
        rt_handle.spawn(async move {
            while let Some(action) = action_rx.recv().await {
                match action {
                    BackendAction::TriggerScan => {
                        let _ = b_actions.trigger_scan().await;
                    }
                    BackendAction::StartPin { paths, hours } => {
                        if let Ok(_rec) = b_actions.start_pin(paths, hours).await {
                            if let Ok(snap) = b_actions.get_status().await {
                                let _ = ev_tx_actions.send(UiEvent::State(snap));
                            }
                        }
                    }
                    BackendAction::StopPin => {
                        if let Ok(_sum) = b_actions.stop_pin().await {
                            if let Ok(snap) = b_actions.get_status().await {
                                let _ = ev_tx_actions.send(UiEvent::State(snap));
                            }
                        }
                    }
                    BackendAction::ReleasePin => {
                        if let Ok(_sum) = b_actions.release_pin().await {
                            if let Ok(snap) = b_actions.get_status().await {
                                let _ = ev_tx_actions.send(UiEvent::State(snap));
                            }
                        }
                    }
                    BackendAction::InitiateShare { i_know } => {
                        let _ = b_actions.share_initiate(None, i_know).await;
                    }
                    BackendAction::AcceptPair { payload_path } => {
                        let _ = b_actions.pair_accept(payload_path, None).await;
                    }
                    BackendAction::FetchStatus => {
                        if let Ok(snap) = b_actions.get_status().await {
                            let _ = ev_tx_actions.send(UiEvent::State(snap));
                        }
                    }
                    BackendAction::FetchConflicts => {
                        if let Ok(confs) = b_actions.list_conflicts().await {
                            for c in confs {
                                let _ = ev_tx_actions.send(UiEvent::ConflictRecorded {
                                    path: c.path,
                                    conflict_path: c.quarantined_as.unwrap_or_default(),
                                    timestamp: 0,
                                    quarantined_as: None,
                                });
                            }
                        }
                    }
                }
                ctx_actions.request_repaint();
            }
        });

        Self {
            backend,
            snapshot: None,
            conflicts: Vec::new(),
            active_pin: None,
            active_share: None,
            share_status: None,
            active_transfer: None,
            activity_log: Vec::new(),
            show_conflicts_modal: false,
            show_pin_modal: false,
            show_share_modal: false,
            show_pair_modal: false,
            pin_paths_input: String::new(),
            pair_path_input: String::new(),
            status_message: None,
            is_connected: true,
            should_quit: false,
            event_rx: Some(event_rx),
            action_tx: Some(action_tx),
        }
    }

    /// Dispatch a user action to the asynchronous worker.
    pub fn dispatch(&self, action: BackendAction) {
        if let Some(ref tx) = self.action_tx {
            let _ = tx.send(action);
        }
    }

    /// Process a typed `UiEvent` into internal UI models.
    pub fn handle_event(&mut self, event: UiEvent) {
        self.is_connected = true;
        match event {
            UiEvent::State(snap) => {
                self.snapshot = Some(snap);
            }
            UiEvent::StateChanged {
                state,
                manifest_id,
                pending_changes,
                stats,
                ..
            } => {
                if let Some(ref mut snap) = self.snapshot {
                    snap.state = state;
                    snap.manifest_id = Some(manifest_id);
                    if let Some(p) = pending_changes {
                        snap.pending_changes = Some(p);
                    }
                    if let Some(s) = stats {
                        snap.scanned = s;
                    }
                }
            }
            UiEvent::TransferProgress {
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            } => {
                if total_bytes > 0 && bytes_transferred >= total_bytes {
                    self.active_transfer = None;
                } else {
                    self.active_transfer = Some(GuiTransferState {
                        bytes_transferred,
                        total_bytes,
                        current_path,
                        chunks_transferred,
                        total_chunks,
                        peer_device_id,
                        direction,
                    });
                }
            }
            UiEvent::ConflictRecorded {
                path,
                conflict_path,
                ..
            } => {
                if !self.conflicts.iter().any(|c| c.path == path) {
                    self.conflicts.push(ConflictEntry {
                        ts: ferry_platform::time::current_time_str(),
                        folder_id: self.snapshot.as_ref().map(|s| s.folder_id.clone()).unwrap_or_default(),
                        path: path.clone(),
                        kind: "content".to_string(),
                        winner: ferry_ipc::protocol::DeviceStamp {
                            device: "remote".to_string(),
                            mtime_sec: None,
                            mtime_nsec: None,
                        },
                        loser: ferry_ipc::protocol::DeviceStamp {
                            device: "local".to_string(),
                            mtime_sec: None,
                            mtime_nsec: None,
                        },
                        quarantined_as: Some(conflict_path),
                    });
                }
            }
            UiEvent::Error { code, message } => {
                self.status_message = Some((
                    format!("{code}: {message}"),
                    Instant::now(),
                    colors::RED_CONFLICT,
                ));
            }
        }
    }

    /// Drain incoming asynchronous events.
    pub fn drain_events(&mut self) {
        let mut events = Vec::new();
        if let Some(ref mut rx) = self.event_rx {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        for e in events {
            self.handle_event(e);
        }
    }

    /// Process keyboard hotkeys.
    pub fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            // Close modal with Escape or 'q'
            if i.key_pressed(Key::Escape) {
                if self.show_conflicts_modal || self.show_pin_modal || self.show_share_modal || self.show_pair_modal {
                    self.show_conflicts_modal = false;
                    self.show_pin_modal = false;
                    self.show_share_modal = false;
                    self.show_pair_modal = false;
                } else {
                    self.should_quit = true;
                }
            }

            // Rescan 'r'
            if i.key_pressed(Key::R) && !i.modifiers.command && !i.modifiers.ctrl {
                self.dispatch(BackendAction::TriggerScan);
                self.status_message = Some(("Rescan triggered".to_string(), Instant::now(), colors::FERRY_GREEN));
            }

            // Pin toggle 'p'
            if i.key_pressed(Key::P) && !i.modifiers.command && !i.modifiers.ctrl {
                let is_pinned = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.pin.holding || s.state.eq_ignore_ascii_case("pinned"));

                if is_pinned {
                    self.dispatch(BackendAction::ReleasePin);
                    self.status_message = Some(("Releasing pin...".to_string(), Instant::now(), colors::FERRY_GREEN));
                } else {
                    self.show_pin_modal = !self.show_pin_modal;
                }
            }

            // Conflicts toggle 'c'
            if i.key_pressed(Key::C) && !i.modifiers.command && !i.modifiers.ctrl {
                self.show_conflicts_modal = !self.show_conflicts_modal;
                if self.show_conflicts_modal {
                    self.dispatch(BackendAction::FetchConflicts);
                }
            }

            // Quit 'q'
            if (i.key_pressed(Key::Q) && i.modifiers.ctrl) || (i.key_pressed(Key::Q) && !self.show_conflicts_modal && !self.show_pin_modal && !self.show_share_modal && !self.show_pair_modal) {
                self.should_quit = true;
            }
        });
    }

    /// Authoritative state badge resolver.
    #[must_use]
    pub fn current_badge(&self) -> (&'static str, Color32, Color32) {
        if !self.is_connected {
            return ("OFFLINE", colors::GRAY_OFFLINE, colors::TEXT_MUTED);
        }
        let Some(ref snap) = self.snapshot else {
            return ("CONNECTING", colors::GRAY_MUTED, colors::TEXT_PRIMARY);
        };

        if snap.pin.holding || snap.state.eq_ignore_ascii_case("pinned") {
            ("PINNED", colors::PURPLE_PINNED, Color32::WHITE)
        } else if snap.conflicts > 0 || !self.conflicts.is_empty() || snap.state.eq_ignore_ascii_case("conflict") {
            ("CONFLICT", colors::RED_CONFLICT, Color32::WHITE)
        } else if snap.state.eq_ignore_ascii_case("syncing") || self.active_transfer.is_some() {
            ("SYNCING", colors::BLUE_SYNCING, Color32::WHITE)
        } else if snap.state.eq_ignore_ascii_case("synced") {
            ("SYNCED", colors::FERRY_GREEN, Color32::BLACK)
        } else if snap.state.eq_ignore_ascii_case("idle") {
            ("IDLE", colors::CARD_BG, colors::TEXT_PRIMARY)
        } else {
            ("OFFLINE", colors::GRAY_OFFLINE, colors::TEXT_MUTED)
        }
    }
}

impl App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Theme::apply(ctx);
        self.drain_events();
        self.handle_shortcuts(ctx);

        if self.should_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Top Navigation Header
        TopBottomPanel::top("top_panel")
            .frame(
                Frame::none()
                    .fill(colors::PANEL_BG)
                    .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                    .inner_margin(Margin::symmetric(16.0f32, 12.0f32)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("⛵ Ferry").strong().color(colors::TEXT_PRIMARY));
                    ui.add_space(8.0);

                    let (badge_text, bg, fg) = self.current_badge();
                    Theme::render_status_badge(ui, badge_text, bg, fg);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Pair").size(12.5)).clicked() {
                            self.show_pair_modal = true;
                        }
                        if ui.button(RichText::new("Share").size(12.5)).clicked() {
                            self.show_share_modal = true;
                        }
                        if ui.button(RichText::new("Conflicts [C]").size(12.5)).clicked() {
                            self.show_conflicts_modal = true;
                            self.dispatch(BackendAction::FetchConflicts);
                        }
                        if ui.button(RichText::new("Pin [P]").size(12.5)).clicked() {
                            self.show_pin_modal = true;
                        }
                        if ui.button(RichText::new("Rescan [R]").size(12.5)).clicked() {
                            self.dispatch(BackendAction::TriggerScan);
                        }
                    });
                });
            });

        // Bottom Shortcut Footer
        TopBottomPanel::bottom("bottom_panel")
            .frame(
                Frame::none()
                    .fill(colors::OBSIDIAN_BG)
                    .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                    .inner_margin(Margin::symmetric(16.0f32, 8.0f32)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("[R] Rescan").size(11.0).color(colors::TEXT_MUTED));
                    ui.label(RichText::new("•").size(11.0).color(colors::GRAY_MUTED));
                    ui.label(RichText::new("[P] Pin").size(11.0).color(colors::TEXT_MUTED));
                    ui.label(RichText::new("•").size(11.0).color(colors::GRAY_MUTED));
                    ui.label(RichText::new("[C] Conflicts").size(11.0).color(colors::TEXT_MUTED));
                    ui.label(RichText::new("•").size(11.0).color(colors::GRAY_MUTED));
                    ui.label(RichText::new("[Q] / [Esc] Quit").size(11.0).color(colors::TEXT_MUTED));

                    if let Some((ref msg, instant, color)) = self.status_message {
                        if instant.elapsed().as_secs() < 5 {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(msg).color(color).size(12.0));
                            });
                        }
                    }
                });
            });

        // Central Content Area
        CentralPanel::default()
            .frame(Frame::none().fill(colors::OBSIDIAN_BG).inner_margin(16.0f32))
            .show(ctx, |ui| {
                ScrollArea::vertical().show(ui, |ui| {
                    if let Some(ref snap) = self.snapshot {
                        // Folder Information Card
                        render_card(ui, "Folder Status", |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Path:").color(colors::TEXT_MUTED));
                                ui.label(RichText::new(&snap.folder).color(colors::TEXT_PRIMARY).strong());
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Folder ID:").color(colors::TEXT_MUTED));
                                ui.monospace(RichText::new(&snap.folder_id).color(colors::TEXT_SECONDARY));
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Manifest:").color(colors::TEXT_MUTED));
                                ui.monospace(
                                    RichText::new(snap.manifest_id.as_deref().unwrap_or("none"))
                                        .color(colors::FERRY_GREEN),
                                );
                            });
                        });

                        ui.add_space(12.0);

                        // Storage Metrics Grid
                        render_card(ui, "Storage & Tree Metrics", |ui| {
                            ui.columns(4, |cols| {
                                cols[0].label(RichText::new("Files").color(colors::TEXT_MUTED).size(11.0));
                                cols[0].heading(RichText::new(format!("{}", snap.scanned.files)).color(colors::TEXT_PRIMARY));

                                cols[1].label(RichText::new("Directories").color(colors::TEXT_MUTED).size(11.0));
                                cols[1].heading(RichText::new(format!("{}", snap.scanned.dirs)).color(colors::TEXT_PRIMARY));

                                cols[2].label(RichText::new("Total Size").color(colors::TEXT_MUTED).size(11.0));
                                cols[2].heading(RichText::new(format_bytes(snap.scanned.bytes_chunked)).color(colors::TEXT_PRIMARY));

                                cols[3].label(RichText::new("Pending Changes").color(colors::TEXT_MUTED).size(11.0));
                                cols[3].heading(RichText::new(format!("{}", snap.pending_changes.unwrap_or(0))).color(colors::TEXT_PRIMARY));
                            });
                        });

                        ui.add_space(12.0);

                        // Active Transfer Banner
                        if let Some(ref transfer) = self.active_transfer {
                            render_card(ui, "Active Transfer", |ui| {
                                let ratio = (transfer.bytes_transferred as f32 / transfer.total_bytes.max(1) as f32).clamp(0.0, 1.0);
                                let dir_str = match transfer.direction {
                                    Some(TransferDirection::Sending) => "Sending",
                                    Some(TransferDirection::Receiving) => "Receiving",
                                    None => "Transferring",
                                };
                                ui.label(RichText::new(format!(
                                    "{dir_str}: {} / {} ({:.0}%)",
                                    format_bytes(transfer.bytes_transferred),
                                    format_bytes(transfer.total_bytes),
                                    ratio * 100.0
                                )).color(colors::BLUE_SYNCING).strong());
                                ui.label(RichText::new(&transfer.current_path).color(colors::TEXT_SECONDARY).size(12.0));
                                ui.add(egui::ProgressBar::new(ratio).fill(colors::BLUE_SYNCING).rounding(Rounding::same(4.0f32)));
                            });
                            ui.add_space(12.0);
                        }

                        // Connected Peers
                        render_card(ui, &format!("Connected Peers ({})", snap.peers.len()), |ui| {
                            if snap.peers.is_empty() {
                                ui.label(RichText::new("No peer devices connected").color(colors::TEXT_MUTED));
                            } else {
                                for p in &snap.peers {
                                    ui.horizontal(|ui| {
                                        ui.monospace(RichText::new(&p.device_id).color(colors::TEXT_PRIMARY));
                                        ui.label(RichText::new(format!("• {}", p.connectivity)).color(colors::TEXT_SECONDARY));
                                        if let Some(ref m) = p.last_agreed_manifest_id {
                                            ui.monospace(RichText::new(format!("agreed: {m}")).color(colors::GRAY_MUTED));
                                        }
                                    });
                                }
                            }
                        });
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(RichText::new("Connecting to Ferry Sync Engine...").color(colors::TEXT_MUTED).size(16.0));
                        });
                    }
                });
            });

        // Modals
        if self.show_conflicts_modal {
            render_modal(ctx, "Quarantined Conflicts", &mut self.show_conflicts_modal, |ui, _open| {
                if self.conflicts.is_empty() {
                    ui.label(RichText::new("No quarantined conflicts detected.").color(colors::TEXT_MUTED));
                } else {
                    for c in &self.conflicts {
                        ui.group(|ui| {
                            ui.label(RichText::new(&c.path).color(colors::RED_CONFLICT).strong());
                            if let Some(ref q) = c.quarantined_as {
                                ui.label(RichText::new(format!("Quarantined as: {q}")).color(colors::TEXT_SECONDARY).size(12.0));
                            }
                            ui.label(RichText::new(format!("Winner: {}", c.winner.device)).color(colors::TEXT_MUTED).size(11.0));
                        });
                        ui.add_space(4.0);
                    }
                }
            });
        }

        if self.show_pin_modal {
            let mut start_pin_action = None;
            let mut paths_input = self.pin_paths_input.clone();
            render_modal(ctx, "Session Pinning", &mut self.show_pin_modal, |ui, open| {
                ui.label(RichText::new("Declare this device the exclusive writer for selected paths:").color(colors::TEXT_SECONDARY));
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut paths_input);
                ui.label(RichText::new("e.g. src/**, tests/** (leave blank for entire folder)").color(colors::TEXT_MUTED).size(11.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Start Pin").color(colors::FERRY_GREEN)).clicked() {
                        let paths: Vec<String> = paths_input.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
                        start_pin_action = Some(BackendAction::StartPin { paths, hours: Some(8) });
                        *open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        *open = false;
                    }
                });
            });
            self.pin_paths_input = paths_input;
            if let Some(act) = start_pin_action {
                self.dispatch(act);
            }
        }

        if self.show_share_modal {
            let mut share_action = None;
            render_modal(ctx, "Share Folder", &mut self.show_share_modal, |ui, open| {
                ui.label(RichText::new("Generate pairing payload and short code for another device:").color(colors::TEXT_SECONDARY));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Generate Offer").color(colors::FERRY_GREEN)).clicked() {
                        share_action = Some(BackendAction::InitiateShare { i_know: true });
                        *open = false;
                    }
                    if ui.button("Close").clicked() {
                        *open = false;
                    }
                });
            });
            if let Some(act) = share_action {
                self.dispatch(act);
            }
        }

        if self.show_pair_modal {
            let mut pair_action = None;
            let mut pair_input = self.pair_path_input.clone();
            render_modal(ctx, "Pair Devices", &mut self.show_pair_modal, |ui, open| {
                ui.label(RichText::new("Accept incoming pairing payload file:").color(colors::TEXT_SECONDARY));
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut pair_input);
                ui.label(RichText::new("Path to .ferry-pair offer file").color(colors::TEXT_MUTED).size(11.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Accept Pair").color(colors::FERRY_GREEN)).clicked() {
                        if !pair_input.trim().is_empty() {
                            pair_action = Some(BackendAction::AcceptPair {
                                payload_path: std::path::PathBuf::from(pair_input.trim()),
                            });
                        }
                        *open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        *open = false;
                    }
                });
            });
            self.pair_path_input = pair_input;
            if let Some(act) = pair_action {
                self.dispatch(act);
            }
        }
    }
}

fn render_card(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    Frame::none()
        .fill(colors::CARD_BG)
        .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
        .rounding(Rounding::same(10.0f32))
        .inner_margin(Margin::same(14.0f32))
        .show(ui, |ui| {
            ui.label(RichText::new(title).strong().color(colors::TEXT_PRIMARY).size(13.5));
            ui.add_space(8.0);
            content(ui);
        });
}

fn render_modal(
    ctx: &egui::Context,
    title: &str,
    is_open: &mut bool,
    content: impl FnOnce(&mut egui::Ui, &mut bool),
) {
    if !*is_open {
        return;
    }
    let mut open = true;
    Area::new(egui::Id::new(title))
        .anchor(Align2::CENTER_CENTER, vec2(0.0f32, 0.0f32))
        .order(Order::Foreground)
        .show(ctx, |ui| {
            Frame::none()
                .fill(colors::CARD_BG)
                .stroke(Stroke::new(1.5f32, colors::GLASS_BORDER_STRONG))
                .rounding(Rounding::same(12.0f32))
                .shadow(Shadow {
                    offset: vec2(0.0f32, 10.0f32),
                    blur: 30.0f32,
                    spread: 0.0f32,
                    color: Color32::from_black_alpha(200),
                })
                .inner_margin(Margin::same(20.0f32))
                .show(ui, |ui| {
                    ui.set_max_width(450.0f32);
                    ui.horizontal(|ui| {
                        ui.heading(RichText::new(title).strong().color(colors::TEXT_PRIMARY));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("✕").clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.separator();
                    ui.add_space(8.0);
                    content(ui, &mut open);
                });
        });
    *is_open = open;
}
