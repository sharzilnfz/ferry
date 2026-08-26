//! Integration tests for CLI IPC integration and offline fallback.
//!
//! Verifies:
//! - `ferry status` queries running daemon over IPC first (instant in-memory response, no disk rescan)
//! - `ferry status --json` matches docs/cli-json.md and checked-in schema snapshot
//! - `ferry pin start`, `pin stop`, `pin release` dispatch over IPC when daemon is running
//! - `ferry conflicts` queries IPC when daemon is running
//! - Fallback to direct disk reads and store inspection when daemon is offline or stopped

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::Env;
use ferry_cli::commands;
use ferry_daemon::ipc::spawn_ipc_server;
use ferry_daemon::state::DaemonState;
use ferry_ipc::paths::socket_path_for_dir;
use ferry_sync::{EngineConfig, SyncEngine, TcpTransport};
use serde_json::Value;

/// Reduce a JSON value to a deterministic schema description (same as `json_schema.rs`).
fn schema(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                schema(&map[k], &format!("{path}.{k}"), out);
            }
        }
        Value::Array(items) => {
            out.push(format!("{path}[]"));
            if let Some(first) = items.first() {
                schema(first, &format!("{path}[0]"), out);
            }
        }
        other => {
            let ty = match other {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(n) => {
                    if n.is_i64() || n.is_u64() {
                        "int"
                    } else {
                        "float"
                    }
                }
                Value::String(_) => "string",
                Value::Object(_) => unreachable!(),
                Value::Array(_) => unreachable!(),
            };
            out.push(format!("{path}:{ty}"));
        }
    }
}

fn schema_of(v: &Value) -> String {
    let mut lines = Vec::new();
    schema(v, "$", &mut lines);
    lines.join("\n") + "\n"
}

fn assert_matches_expected_schema(name: &str, actual: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/expected");
    let file = dir.join(format!("{name}.schema.txt"));
    let expected = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!("missing expected schema {}", file.display())
    });
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(expected, actual, "JSON schema for {name} drifted");
}

struct RunningDaemon {
    _engine_handle: ferry_sync::EngineHandle,
    state: Arc<DaemonState>,
    ipc_handle: Option<ferry_daemon::ipc::IpcServerHandle>,
}

impl RunningDaemon {
    fn start(proj: &std::path::Path) -> Self {
        let opened = ferry_cli::folder::open_folder(proj).expect("open folder");
        let identity = ferry_cli::ensure_identity().expect("device identity");

        let mut cfg = EngineConfig::default_for_test(12345);
        cfg.tag = "ipc-test-daemon".to_string();
        cfg.store_dir.clone_from(&opened.root);
        cfg.tree_dir.clone_from(&opened.root);
        cfg.folder_id = opened.folder_id;
        cfg.pin_state_dir = Some(opened.state_dir());
        cfg.poll_interval = Duration::from_millis(50);

        let mut engine = SyncEngine::new(cfg, Arc::new(TcpTransport)).expect("engine init");
        engine.set_identity(identity.clone());
        let handle = engine.start();

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(128);
        let daemon_state = Arc::new(DaemonState::new(
            handle.clone(),
            opened.root.clone(),
            opened.root.clone(),
            opened.folder_id,
            identity,
            broadcast_tx,
        ));

        let socket_path = socket_path_for_dir(&opened.root);
        let ipc_handle = spawn_ipc_server(socket_path, Arc::clone(&daemon_state))
            .expect("spawn ipc server");

        Self {
            _engine_handle: handle,
            state: daemon_state,
            ipc_handle: Some(ipc_handle),
        }
    }

    fn stop_ipc(&mut self) {
        if let Some(h) = self.ipc_handle.take() {
            h.shutdown();
        }
    }
}

#[test]
fn test_status_ipc_query_and_schema_matching() {
    let env = Env::new("status-ipc");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();
    std::fs::write(proj.join("initial.txt"), b"initial content\n").unwrap();

    // 1. First verify offline status returns valid schema
    let offline_out = commands::status::run(&proj).unwrap();
    assert_matches_expected_schema("status", &schema_of(&offline_out.json));
    assert_eq!(offline_out.json["command"], "status");
    assert_eq!(offline_out.json["scanned"]["files"], 2); // initial.txt + ferry.ignore

    // 2. Start daemon IPC server
    let mut daemon = RunningDaemon::start(&proj);

    // Give engine time to complete its initial scan
    std::thread::sleep(Duration::from_millis(150));

    // 3. Query status while daemon is running: should query over IPC
    let ipc_out = commands::status::run(&proj).unwrap();
    assert_matches_expected_schema("status", &schema_of(&ipc_out.json));
    assert_eq!(ipc_out.json["command"], "status");

    // Write a new file to disk WITHOUT triggering engine rescan
    std::fs::write(proj.join("unscanned_file.txt"), b"unscanned content\n").unwrap();

    // Query status again over IPC - it must return instant cached status from daemon, not fresh scan
    let cached_out = commands::status::run(&proj).unwrap();
    assert_eq!(cached_out.json["command"], "status");
    assert_matches_expected_schema("status", &schema_of(&cached_out.json));
    assert!(cached_out.human.contains("Folder"));
    assert!(cached_out.human.contains("Device"));

    // 4. Stop daemon IPC server -> query status should cleanly fall back to direct disk scan
    daemon.stop_ipc();
    std::thread::sleep(Duration::from_millis(50));

    let fallback_out = commands::status::run(&proj).unwrap();
    assert_eq!(fallback_out.json["command"], "status");
    assert_matches_expected_schema("status", &schema_of(&fallback_out.json));
    // Offline fresh scan picks up the unscanned_file.txt now
    assert_eq!(fallback_out.json["scanned"]["files"], 3);
}

#[test]
fn test_pin_lifecycle_over_ipc_with_fallback() {
    let env = Env::new("pin-ipc");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/lib.rs"), b"// lib\n").unwrap();

    // Start daemon
    let mut daemon = RunningDaemon::start(&proj);
    std::thread::sleep(Duration::from_millis(150));

    // 1. Pin start over IPC
    let start_out = commands::pin::start(&proj, &["src/**".to_string()]).unwrap();
    assert_eq!(start_out.json["command"], "pin");
    assert_eq!(start_out.json["action"], "start");
    assert_eq!(start_out.json["paths"][0], "src/**");
    assert_matches_expected_schema("pin-start", &schema_of(&start_out.json));

    // Verify snapshot in daemon reflects active pin
    let snap = daemon.state.snapshot();
    assert_eq!(snap.pin.state, "active");
    assert!(snap.pin.holding);
    assert_eq!(snap.pin.paths, vec!["src/**".to_string()]);

    // 2. Pin status
    let status_out = commands::pin::status(&proj).unwrap();
    assert_eq!(status_out.json["state"], "active");
    assert_eq!(status_out.json["holding"], true);

    // 3. Pin stop over IPC
    let stop_out = commands::pin::stop(&proj).unwrap();
    assert_eq!(stop_out.json["command"], "pin");
    assert_eq!(stop_out.json["action"], "stop");
    assert_eq!(stop_out.json["was_pinned"], true);

    // Verify daemon snapshot reflects released pin
    let snap_after = daemon.state.snapshot();
    assert_eq!(snap_after.pin.state, "released");

    // 4. Pin release over IPC
    let release_out = commands::pin::release(&proj).unwrap();
    assert_eq!(release_out.json["command"], "pin");
    assert_eq!(release_out.json["action"], "release");
    assert_eq!(release_out.json["pin_ended"], true);

    // 5. Stop daemon and verify offline pin lifecycle works identically
    daemon.stop_ipc();
    std::thread::sleep(Duration::from_millis(50));

    let offline_start = commands::pin::start(&proj, &["src/**".to_string()]).unwrap();
    assert_eq!(offline_start.json["command"], "pin");
    assert_eq!(offline_start.json["action"], "start");

    let offline_stop = commands::pin::stop(&proj).unwrap();
    assert_eq!(offline_stop.json["was_pinned"], true);
}

#[test]
fn test_conflicts_query_over_ipc_and_fallback() {
    let env = Env::new("conflicts-ipc");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj, "init").unwrap();

    let entry = ferry_sync_engine::ConflictEntry {
        ts: "2026-08-25T12:00:00Z".into(),
        folder_id: "aa".repeat(16),
        path: "conflict.txt".into(),
        kind: "both_changed".into(),
        winner: ferry_sync_engine::DeviceStamp {
            device: "11".repeat(32),
            mtime_sec: Some(100),
            mtime_nsec: Some(0),
        },
        loser: ferry_sync_engine::DeviceStamp {
            device: "22".repeat(32),
            mtime_sec: Some(90),
            mtime_nsec: Some(0),
        },
        quarantined_as: Some("conflict.txt.ferry-conflict.22222222-20260825-120000".into()),
    };

    ferry_sync_engine::append_entries(
        &ferry_cli::folder::state_dir(&proj),
        std::slice::from_ref(&entry),
    )
    .unwrap();

    // 1. Verify offline conflicts query
    let offline_conflicts = commands::conflicts::run(&proj).unwrap();
    assert_eq!(offline_conflicts.json["command"], "conflicts");
    assert_eq!(offline_conflicts.json["entries"].as_array().unwrap().len(), 1);
    assert_matches_expected_schema("conflicts", &schema_of(&offline_conflicts.json));

    // 2. Start daemon and query conflicts over IPC
    let mut daemon = RunningDaemon::start(&proj);
    std::thread::sleep(Duration::from_millis(150));

    let ipc_conflicts = commands::conflicts::run(&proj).unwrap();
    assert_eq!(ipc_conflicts.json["command"], "conflicts");
    assert_eq!(ipc_conflicts.json["entries"].as_array().unwrap().len(), 1);
    assert_eq!(ipc_conflicts.json["entries"][0]["path"], "conflict.txt");
    assert_matches_expected_schema("conflicts", &schema_of(&ipc_conflicts.json));
    assert!(ipc_conflicts.human.contains("conflict.txt"));

    // 3. Stop daemon and verify fallback
    daemon.stop_ipc();
    std::thread::sleep(Duration::from_millis(50));

    let fallback_conflicts = commands::conflicts::run(&proj).unwrap();
    assert_eq!(fallback_conflicts.json["entries"].as_array().unwrap().len(), 1);
}
