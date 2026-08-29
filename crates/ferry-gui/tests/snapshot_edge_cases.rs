//! Snapshot test scenarios verifying UI edge cases and boundary conditions.
//!
//! Tests:
//! 1. Complex & asymmetric conflicts (nested paths, subsecond mtimes, deletion kinds)
//! 2. Multi-peer held pins with wildcard glob patterns
//! 3. Active transfers with boundary values (0-byte, 100GB, missing optional fields)
//! 4. Secret scan warning detection, red warning modal rendering, and user override flow

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Context, RawInput};
use ferry_gui::beacon::BeaconState;
use ferry_gui::format_bytes;
use ferry_gui::modals::render_share_modal;
use ferry_gui::theme::Theme;
use ferry_gui::GuiApp;
use ferry_ipc::backend::{FakeBackend, SessionDomain, UiEvent};
use ferry_ipc::protocol::{ConflictEntry, DeviceStamp, EngineSnapshot, PinView, TransferDirection};

#[test]
fn test_edge_case_asymmetric_conflicts() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);

    // Conflict with deep path, asymmetric nano timestamps, and deletion conflict
    let deep_conflict = ConflictEntry {
        ts: "2026-08-28T03:25:00.123456789Z".to_string(),
        folder_id: "deep-repo-root".to_string(),
        path: "packages/core/submodules/service/v2/nested/config.toml".to_string(),
        kind: "delete_vs_edit".to_string(),
        winner: DeviceStamp {
            device: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            mtime_sec: Some(1787575123),
            mtime_nsec: Some(999888777),
        },
        loser: DeviceStamp {
            device: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".to_string(),
            mtime_sec: Some(1787575120),
            mtime_nsec: Some(111222333),
        },
        quarantined_as: Some(
            "packages/core/submodules/service/v2/nested/config.toml.ferry-conflict.fedcba98-1787575120".to_string(),
        ),
    };

    app.conflicts.push(deep_conflict.clone());
    app.show_conflicts_modal = true;

    let ctx = Context::default();
    Theme::apply(&ctx);

    // Verify modal renders without clipping or panic
    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });

    assert_eq!(app.conflicts.len(), 1);
    assert_eq!(
        app.conflicts[0].path,
        "packages/core/submodules/service/v2/nested/config.toml"
    );
}

#[test]
fn test_edge_case_held_pins_and_multi_peer_holds() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);

    let mut snap = EngineSnapshot::new("/workspace/project", "f-123", "d-456", "pinned");
    snap.pin = PinView {
        state: "pinned".to_string(),
        holding: true,
        paths: vec![
            "**/*.lock".to_string(),
            "target/**".to_string(),
            "config/secrets.json".to_string(),
        ],
    };
    snap.held_changes = 42;

    let mut held_map = HashMap::new();
    held_map.insert(
        "peer-alpha".to_string(),
        vec!["Cargo.lock".to_string(), "package-lock.json".to_string()],
    );
    held_map.insert(
        "peer-beta".to_string(),
        vec!["target/debug/bin".to_string()],
    );
    snap.held_by_peer = held_map;

    app.handle_event(UiEvent::State(snap));

    assert_eq!(app.beacon_state(), BeaconState::Holding);
    assert_eq!(app.current_badge().0, "HOLDING");
    assert_eq!(app.snapshot.as_ref().unwrap().held_changes, 42);
    assert_eq!(app.snapshot.as_ref().unwrap().pin.paths.len(), 3);
    assert_eq!(app.snapshot.as_ref().unwrap().held_by_peer.len(), 2);

    let ctx = Context::default();
    Theme::apply(&ctx);
    app.show_pin_modal = true;

    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });
}

#[test]
fn test_edge_case_active_transfers_boundary_values() {
    // 1. Zero total bytes edge case
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);
    app.handle_event(UiEvent::State(EngineSnapshot::new(
        "/workspace/project",
        "f-1",
        "d-1",
        "synced",
    )));

    app.handle_event(UiEvent::TransferProgress {
        bytes_transferred: 0,
        total_bytes: 0,
        current_path: "empty_marker.txt".to_string(),
        chunks_transferred: None,
        total_chunks: None,
        peer_device_id: None,
        direction: None,
    });

    assert_eq!(app.beacon_state(), BeaconState::Syncing);
    let transfer = app.active_transfer.as_ref().unwrap();
    assert_eq!(transfer.bytes_transferred, 0);
    assert_eq!(transfer.total_bytes, 0);

    let ctx = Context::default();
    Theme::apply(&ctx);
    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });

    // 2. 120 GB massive transfer edge case
    assert_eq!(format_bytes(120 * 1024 * 1024 * 1024), "120.00 GB");

    app.handle_event(UiEvent::TransferProgress {
        bytes_transferred: 60 * 1024 * 1024 * 1024,
        total_bytes: 120 * 1024 * 1024 * 1024,
        current_path: "datasets/raw_dump.iso".to_string(),
        chunks_transferred: Some(60_000),
        total_chunks: Some(120_000),
        peer_device_id: Some("datacenter-node-01".to_string()),
        direction: Some(TransferDirection::Receiving),
    });

    let transfer2 = app.active_transfer.as_ref().unwrap();
    assert_eq!(transfer2.bytes_transferred, 60 * 1024 * 1024 * 1024);
    assert_eq!(transfer2.direction, Some(TransferDirection::Receiving));

    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });
}

#[tokio::test]
async fn test_edge_case_secret_scan_warnings_and_override() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake.clone());

    // Populate detected secrets in folder
    let warnings = vec![
        ".env: line 4 [AWS Access Key ID]".to_string(),
        "id_ed25519: line 1 [Private SSH Key]".to_string(),
        "config/creds.json: line 12 [Bearer Token]".to_string(),
    ];

    app.share_secret_warnings = warnings.clone();
    app.show_share_modal = true;
    app.share_override_secrets = false;

    let ctx = Context::default();
    Theme::apply(&ctx);

    // Initial render: override checkbox is unchecked
    let _ = ctx.run(RawInput::default(), |ctx| {
        render_share_modal(
            ctx,
            &mut app.show_share_modal,
            app.active_share.as_ref(),
            &app.share_secret_warnings,
            &mut app.share_override_secrets,
            |_| {},
        );
    });

    assert!(!app.share_override_secrets);
    assert!(app.active_share.is_none());

    // User acknowledges risk and checks override
    app.share_override_secrets = true;

    let offer = fake
        .share_initiate(Some(PathBuf::from("/workspace/secrets-folder")), true)
        .await
        .unwrap();

    assert_eq!(offer.folder, "/workspace/secrets-folder");
    assert!(!offer.token.is_empty());
    assert!(offer.qr_payload.is_some());

    app.active_share = Some(offer.clone());

    // Render modal with active offer
    let _ = ctx.run(RawInput::default(), |ctx| {
        render_share_modal(
            ctx,
            &mut app.show_share_modal,
            app.active_share.as_ref(),
            &app.share_secret_warnings,
            &mut app.share_override_secrets,
            |_| {},
        );
    });

    assert!(app.active_share.is_some());
    assert_eq!(app.active_share.as_ref().unwrap().token, offer.token);
}
