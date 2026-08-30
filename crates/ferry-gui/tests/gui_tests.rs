

use std::sync::Arc;

use egui::{Context, RawInput};
use ferry_gui::activity::{render_activity_stream, ActivityEntry};
use ferry_gui::beacon::{status_beacon_ui, BeaconState};
use ferry_gui::fleet::render_fleet_table;
use ferry_gui::modals::{
    generate_ascii_qr, render_conflicts_modal, render_pair_modal, render_pin_modal,
    render_share_modal,
};
use ferry_gui::telemetry::{format_short_hex, render_telemetry_hairline};
use ferry_gui::theme::{colors, Theme};
use ferry_gui::{format_bytes, BackendAction, GuiApp};
use ferry_ipc::backend::{FakeBackend, ShareOffer, UiEvent};
use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, EngineSnapshot, PeerStatusView, ScanStatsView, TransferDirection,
};

#[test]
fn test_theme_application_and_tokens() {
    let ctx = Context::default();
    Theme::apply(&ctx);

    let visuals = ctx.style().visuals.clone();
    assert!(visuals.dark_mode);
    assert_eq!(visuals.panel_fill, colors::OBSIDIAN_BG);
    assert_eq!(visuals.window_fill, colors::CARD_BG);
}

#[test]
fn test_gui_format_bytes() {
    assert_eq!(format_bytes(500), "500 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1024 * 1024 * 5), "5.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 1024 * 2), "2.00 GB");
}

#[test]
fn test_format_short_hex() {
    assert_eq!(format_short_hex(None), "none");
    assert_eq!(format_short_hex(Some("")), "none");
    assert_eq!(format_short_hex(Some("12345678")), "12345678");
    assert_eq!(
        format_short_hex(Some("0123456789abcdef0123456789abcdef")),
        "012345…cdef"
    );
}

#[test]
fn test_beacon_states_and_pulses() {
    assert_eq!(BeaconState::Synced.color(), colors::FERRY_GREEN);
    assert_eq!(BeaconState::Syncing.color(), colors::BLUE_SYNCING);
    assert_eq!(BeaconState::Holding.color(), colors::PURPLE_PINNED);
    assert_eq!(BeaconState::Conflict.color(), colors::RED_CONFLICT);
    assert_eq!(BeaconState::Offline.color(), colors::GRAY_OFFLINE);

    assert_eq!(BeaconState::Synced.label(), "SYNCED");
    assert_eq!(BeaconState::Syncing.label(), "SYNCING");
    assert_eq!(BeaconState::Holding.label(), "HOLDING");
    assert_eq!(BeaconState::Conflict.label(), "CONFLICT");

    assert!(BeaconState::Syncing.pulse_speed() > BeaconState::Synced.pulse_speed());
    assert!(BeaconState::Conflict.pulse_speed() > BeaconState::Holding.pulse_speed());

    
    let ctx = Context::default();
    let _ = ctx.run(RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            status_beacon_ui(ui, BeaconState::Synced, 1.23);
            status_beacon_ui(ui, BeaconState::Syncing, 2.34);
            status_beacon_ui(ui, BeaconState::Holding, 3.45);
            status_beacon_ui(ui, BeaconState::Conflict, 4.56);
        });
    });
}

#[test]
fn test_telemetry_hairline_rendering() {
    let ctx = Context::default();
    let mut snap = EngineSnapshot::new("/test/folder", "folder123", "device456", "synced");
    snap.manifest_id =
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());
    snap.held_changes = 3;

    let mut conflicts_clicked = false;

    let _ = ctx.run(RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            render_telemetry_hairline(ui, Some(&snap), 2, || {
                conflicts_clicked = true;
            });
        });
    });
}

#[test]
fn test_fleet_table_rendering() {
    let ctx = Context::default();
    let mut peer1 = PeerStatusView::new("0123456789abcdef0123456789abcdef", "online");
    peer1.agreed_at = Some("2026-08-28T03:00:00Z".to_string());

    let mut peer2 = PeerStatusView::new("fedcba9876543210fedcba9876543210", "dialing");
    peer2.last_agreed_manifest_id = Some("a1b2c3d4e5f6".to_string());

    let peer3 = PeerStatusView::new("99999999999999999999999999999999", "offline");

    let peers = vec![peer1, peer2, peer3];
    let mut pair_clicked = false;
    let mut share_clicked = false;

    let _ = ctx.run(RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            render_fleet_table(ui, &peers, || pair_clicked = true, || share_clicked = true);
        });
    });
}

#[test]
fn test_activity_stream_logging() {
    let log = vec![
        ActivityEntry::new("Scan", "Scanned 150 files", colors::FERRY_GREEN),
        ActivityEntry::new("Transfer", "Sending data/large.bin", colors::BLUE_SYNCING),
        ActivityEntry::new("Conflict", "Quarantined doc.txt", colors::RED_CONFLICT),
    ];

    assert_eq!(log.len(), 3);
    assert_eq!(log[0].category, "Scan");
    assert_eq!(log[1].category, "Transfer");
    assert_eq!(log[2].category, "Conflict");

    let ctx = Context::default();
    let mut auto_scroll = true;
    let mut cleared = false;

    let _ = ctx.run(RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            render_activity_stream(ui, &log, &mut auto_scroll, || cleared = true);
        });
    });
}

#[test]
fn test_qr_generation() {
    let qr = generate_ascii_qr("FERRY:PAIR:TEST:1234567890");
    assert!(!qr.is_empty());
}

#[test]
fn test_modals_render() {
    let ctx = Context::default();

    
    let mut show_conflicts = true;
    let conflicts = vec![ConflictEntry {
        ts: "2026-08-28T03:10:00Z".to_string(),
        folder_id: "folder123".to_string(),
        path: "src/main.rs".to_string(),
        kind: "content".to_string(),
        winner: DeviceStamp {
            device: "deviceA".to_string(),
            mtime_sec: Some(1787574896),
            mtime_nsec: None,
        },
        loser: DeviceStamp {
            device: "deviceB".to_string(),
            mtime_sec: Some(1787574890),
            mtime_nsec: None,
        },
        quarantined_as: Some("src/main.rs.ferry-conflict.deviceB-1787574890".to_string()),
    }];

    let _ = ctx.run(RawInput::default(), |ctx| {
        render_conflicts_modal(ctx, &mut show_conflicts, &conflicts, || {});
    });

    
    let mut show_share = true;
    let warnings = vec![".env: line 1 [Private Key]".to_string()];
    let mut override_secrets = false;

    let _ = ctx.run(RawInput::default(), |ctx| {
        render_share_modal(
            ctx,
            &mut show_share,
            None,
            &warnings,
            &mut override_secrets,
            |_| {},
        );
    });

    
    let offer = ShareOffer {
        folder: "/test/folder".to_string(),
        token: "ABCD-EFGH-IJKL-MNOP-QRST-UVWX".to_string(),
        payload_path: Some(std::path::PathBuf::from(
            "/test/folder/.ferry/pair-offer.ferry-pair",
        )),
        qr_payload: Some("FERRY:PAIR:TEST".to_string()),
        expires_at: None,
        secret_warnings: Vec::new(),
    };

    let _ = ctx.run(RawInput::default(), |ctx| {
        render_share_modal(
            ctx,
            &mut show_share,
            Some(&offer),
            &[],
            &mut override_secrets,
            |_| {},
        );
    });

    
    let mut show_pair = true;
    let mut pair_input = "/tmp/pair-offer.ferry-pair".to_string();
    let _ = ctx.run(RawInput::default(), |ctx| {
        render_pair_modal(ctx, &mut show_pair, &mut pair_input, |_| {});
    });

    
    let mut show_pin = true;
    let mut pin_input = "src/**".to_string();
    let _ = ctx.run(RawInput::default(), |ctx| {
        render_pin_modal(ctx, &mut show_pin, &mut pin_input, |_, _| {});
    });
}

#[test]
fn test_gui_app_full_lifecycle() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);

    
    assert_eq!(app.current_badge().0, "OFFLINE");
    assert_eq!(app.beacon_state(), BeaconState::Offline);

    
    let mut snap = EngineSnapshot::new("/test/folder", "folder123", "device456", "synced");
    snap.scanned = ScanStatsView::new(100, 20, 0, 10_000_000);
    snap.peers.push(PeerStatusView::new("peer-1", "online"));
    app.handle_event(UiEvent::State(snap));

    assert_eq!(app.current_badge().0, "SYNCED");
    assert_eq!(app.beacon_state(), BeaconState::Synced);
    assert_eq!(app.snapshot.as_ref().unwrap().folder, "/test/folder");
    assert_eq!(app.snapshot.as_ref().unwrap().scanned.files, 100);
    assert_eq!(app.activity_log.len(), 1);

    
    app.handle_event(UiEvent::TransferProgress {
        bytes_transferred: 5_000_000,
        total_bytes: 10_000_000,
        current_path: "data/file.bin".to_string(),
        chunks_transferred: Some(50),
        total_chunks: Some(100),
        peer_device_id: Some("peer1".to_string()),
        direction: Some(TransferDirection::Sending),
    });

    assert_eq!(app.current_badge().0, "SYNCING");
    assert_eq!(app.beacon_state(), BeaconState::Syncing);
    assert!(app.active_transfer.is_some());

    
    app.handle_event(UiEvent::ConflictRecorded {
        path: "src/main.rs".to_string(),
        conflict_path: "src/main.rs.ferry-conflict.peer1-1787574890".to_string(),
        timestamp: 1787574896,
        quarantined_as: Some("src/main.rs.ferry-conflict.peer1-1787574890".to_string()),
    });

    assert_eq!(app.current_badge().0, "CONFLICT");
    assert_eq!(app.beacon_state(), BeaconState::Conflict);
    assert_eq!(app.conflicts.len(), 1);

    
    app.handle_event(UiEvent::FolderRegistered {
        path: "/test/registered/folder".to_string(),
    });
    assert!(app
        .status_message
        .as_ref()
        .is_some_and(
            |(msg, _, color)| msg == "Folder added: /test/registered/folder"
                && *color == colors::FERRY_GREEN
        ));
    assert!(app
        .activity_log
        .iter()
        .any(|entry| entry.category == "Folder"
            && entry.message == "Folder added: /test/registered/folder"
            && entry.color == colors::FERRY_GREEN));

    
    let ctx = Context::default();
    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });
}

async fn wait_for_status_banner(app: &mut GuiApp, needle: &str) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        app.drain_events();
        if let Some((msg, _, _)) = &app.status_message {
            if msg.contains(needle) {
                return true;
            }
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn register_uninitialized_folder_is_blocked_with_init_banner() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new(fake, Context::default(), tokio::runtime::Handle::current());
    let dir = tempfile::tempdir().unwrap();

    app.dispatch(BackendAction::RegisterFolder {
        path: dir.path().to_path_buf(),
    });

    let blocked = wait_for_status_banner(&mut app, "ferry init").await;
    assert!(blocked, "guard banner pointing at `ferry init` must appear");
    assert!(
        app.status_message
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("not an initialized Ferry folder")),
        "banner names the uninitialized directory"
    );
}

#[tokio::test]
async fn register_initialized_folder_reaches_backend_unchanged() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new(fake, Context::default(), tokio::runtime::Handle::current());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ferry")).unwrap();
    std::fs::write(dir.path().join(".ferry").join("config"), b"head").unwrap();

    app.dispatch(BackendAction::RegisterFolder {
        path: dir.path().to_path_buf(),
    });

    
    
    let reached = wait_for_status_banner(&mut app, "not-implemented").await;
    assert!(reached, "initialized path must reach the backend");
    assert!(
        !app.status_message
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("not an initialized Ferry folder")),
        "initialized path must not be blocked"
    );
}

struct SuccessRegisterBackend {
    fake: FakeBackend,
}

impl ferry_ipc::backend::StatusDomain for SuccessRegisterBackend {
    fn get_status(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<EngineSnapshot, ferry_ipc::backend::OpError>>
    {
        self.fake.get_status()
    }
    fn list_conflicts(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<Vec<ConflictEntry>, ferry_ipc::backend::OpError>>
    {
        self.fake.list_conflicts()
    }
    fn trigger_scan(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<(), ferry_ipc::backend::OpError>> {
        self.fake.trigger_scan()
    }
    fn subscribe_events(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::UiEventStream, ferry_ipc::backend::OpError>,
    > {
        self.fake.subscribe_events()
    }
}

impl ferry_ipc::backend::InventoryDomain for SuccessRegisterBackend {
    fn list_directory(
        &self,
        path: Option<std::path::PathBuf>,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::ListDirectoryResponse, ferry_ipc::backend::OpError>,
    > {
        self.fake.list_directory(path)
    }
    fn list_folders(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<Vec<ferry_ipc::FolderRecord>, ferry_ipc::backend::OpError>,
    > {
        self.fake.list_folders()
    }
    fn register_folder(
        &self,
        path: std::path::PathBuf,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::FolderRecord, ferry_ipc::backend::OpError>,
    > {
        Box::pin(async move {
            Ok(ferry_ipc::FolderRecord {
                folder_id: "0123456789abcdef0123456789abcdef".to_string(),
                path,
                added_at: "2026-08-30T12:00:00Z".to_string(),
            })
        })
    }
    fn remove_folder(
        &self,
        folder_id: String,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<(), ferry_ipc::backend::OpError>> {
        self.fake.remove_folder(folder_id)
    }
}

impl ferry_ipc::backend::SessionDomain for SuccessRegisterBackend {
    fn start_pin(
        &self,
        paths: Vec<String>,
        hours: Option<u64>,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::PinRecord, ferry_ipc::backend::OpError>,
    > {
        self.fake.start_pin(paths, hours)
    }
    fn stop_pin(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::PinStopSummary, ferry_ipc::backend::OpError>,
    > {
        self.fake.stop_pin()
    }
    fn release_pin(
        &self,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::PinReleaseSummary, ferry_ipc::backend::OpError>,
    > {
        self.fake.release_pin()
    }
    fn share_initiate(
        &self,
        folder: Option<std::path::PathBuf>,
        i_know: bool,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::ShareOffer, ferry_ipc::backend::OpError>,
    > {
        self.fake.share_initiate(folder, i_know)
    }
    fn share_status(
        &self,
        folder: Option<std::path::PathBuf>,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::backend::ShareStatus, ferry_ipc::backend::OpError>,
    > {
        self.fake.share_status(folder)
    }
    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<std::path::PathBuf>,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<ferry_ipc::PairResult, ferry_ipc::backend::OpError>>
    {
        self.fake.pair_accept(code_or_payload, dir)
    }
    fn create_pairing_session(
        &self,
        req: ferry_ipc::CreatePairingRequest,
    ) -> ferry_ipc::backend::BoxFuture<
        '_,
        Result<ferry_ipc::CreatePairingResponse, ferry_ipc::backend::OpError>,
    > {
        self.fake.create_pairing_session(req)
    }
    fn join_pairing_session(
        &self,
        req: ferry_ipc::JoinPairingRequest,
    ) -> ferry_ipc::backend::BoxFuture<'_, Result<ferry_ipc::PairResult, ferry_ipc::backend::OpError>>
    {
        self.fake.join_pairing_session(req)
    }
}

#[tokio::test]
async fn register_initialized_folder_success_emits_typed_folder_registered_event() {
    let mock = Arc::new(SuccessRegisterBackend {
        fake: FakeBackend::new(),
    });
    let mut app = GuiApp::new(mock, Context::default(), tokio::runtime::Handle::current());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ferry")).unwrap();
    std::fs::write(dir.path().join(".ferry").join("config"), b"head").unwrap();

    let target_path = dir.path().to_path_buf();
    app.dispatch(BackendAction::RegisterFolder {
        path: target_path.clone(),
    });

    let success_message = format!("Folder added: {}", target_path.display());
    let reached = wait_for_status_banner(&mut app, &success_message).await;
    assert!(
        reached,
        "successful registration must show Folder added banner"
    );
    assert_eq!(app.status_message.as_ref().unwrap().2, colors::FERRY_GREEN);
    assert!(app
        .activity_log
        .iter()
        .any(|entry| entry.category == "Folder"
            && entry.message == success_message
            && entry.color == colors::FERRY_GREEN));
}
