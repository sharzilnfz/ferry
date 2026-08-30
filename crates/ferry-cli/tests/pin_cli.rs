//! Functional lifecycle tests for `ferry pin`: start/stop/release/status
//! through the library surface, including JSON shapes, error codes, stale
//! pins, and glob scoping. Isolated `FERRY_HOME` per test (see common/mod.rs).

mod common;

use common::{Env, RunningDaemon};
use ferry_cli::commands;
use ferry_pin::{HeldEntry, HeldLedger};
use ferry_store::format::BlobKind;
use std::collections::BTreeMap;

fn setup() -> (Env, std::path::PathBuf, RunningDaemon) {
    let env = Env::new("pin-cli");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj).unwrap();
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/a.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(proj.join("README.md"), b"hi\n").unwrap();
    let daemon = RunningDaemon::start(&proj);
    (env, proj, daemon)
}

/// Put one real held entry on the ledger, backed by a manifest that IS in
/// the store (release refuses fabricated references loudly).
fn hold_one_real_change(proj: &std::path::Path, peer_hex: &str, path: &str) {
    let opened = ferry_cli::folder::open_folder(proj).unwrap();
    let scan = ferry_cli::commands::status::scan_now(&opened).unwrap();
    opened
        .store
        .put_meta(BlobKind::Manifest, &scan.manifest_bytes)
        .unwrap();
    opened.store.flush().unwrap();
    opened.store.write_index_snapshot().unwrap();

    let ledger = HeldLedger::new(ferry_cli::folder::state_dir(proj));
    ledger
        .append(
            peer_hex,
            &[HeldEntry {
                held_sec: 1_787_574_000,
                held_nsec: 0,
                path: path.into(),
                device_id: peer_hex.to_string(),
                remote_manifest_id: ferry_store::format::hex(&scan.manifest_id),
                chunks: vec![],
                decision: "conflict".into(),
                conflict_winner: Some("local".into()),
            }],
        )
        .unwrap();
}

#[test]
fn full_lifecycle_with_json_shapes() {
    let (_e, proj, _daemon) = setup();

    // Nothing pinned yet.
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["command"], "pin");
    assert_eq!(st.json["action"], "status");
    assert_eq!(st.json["state"], "none");
    assert_eq!(st.json["holding"], false);
    assert_eq!(st.json["held_changes"], 0);

    // Start scoped to src/**.
    let out = commands::pin::start(&proj, &["src/**".to_string()], 8).unwrap();
    assert_eq!(out.json["action"], "start");
    assert_eq!(out.json["paths"][0], "src/**");
    assert!(out.json["pid"].as_u64().is_some());
    let device = out.json["device_id"].as_str().unwrap().to_string();
    assert_eq!(device.len(), 64);

    // A second start is refused while the first holds.
    let err = commands::pin::start(&proj, &[], 8).unwrap_err();
    assert_eq!(err.code, "pin-active");

    // Status reflects an active, holding pin.
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "active");
    assert_eq!(st.json["holding"], true);

    // A peer's change arrives and is held; status surfaces the held set.
    let peer = "b".repeat(32);
    hold_one_real_change(&proj, &peer, "src/a.rs");
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["held_changes"], 1);
    assert_eq!(
        st.json["held_by_peer"][&peer][0].as_str().unwrap(),
        "src/a.rs"
    );

    // Stop ends the session but keeps the ledger recoverable.
    let stopped = commands::pin::stop(&proj).unwrap();
    assert_eq!(stopped.json["was_pinned"], true);
    assert_eq!(stopped.json["held_changes"], 1);
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "released");

    // Release reconciles the ledger through the three-way engine.
    let rel = commands::pin::release(&proj).unwrap();
    assert_eq!(rel.json["action"], "release");
    let peers = rel.json["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["device_id"], peer.as_str());
    assert_eq!(peers[0]["held_paths"][0], "src/a.rs");
    assert_eq!(rel.json["pin_ended"], true);

    // Ledger cleared: second release is a documented no-op.
    let again = commands::pin::release(&proj).unwrap();
    assert_eq!(again.json["peers"].as_array().unwrap().len(), 0);
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["held_changes"], 0);

    // And a fresh session may begin.
    commands::pin::start(&proj, &[], 8).unwrap(); // whole-folder default ("*")
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "active");
    assert_eq!(st.json["paths"][0], "*");
}

#[test]
fn release_with_no_ledgers_is_a_noop_document() {
    let (_e, proj, _daemon) = setup();
    commands::pin::start(&proj, &[], 8).unwrap();
    let rel = commands::pin::release(&proj).unwrap();
    assert_eq!(rel.json["peers"].as_array().unwrap().len(), 0);
    assert_eq!(rel.json["quarantined"], 0);
    assert_eq!(rel.json["conflicts_recorded"], 0);
}

#[test]
fn bad_glob_refused_before_any_state_is_written() {
    let (_e, proj, _daemon) = setup();
    let err = commands::pin::start(&proj, &["[z-a]".to_string()], 8).unwrap_err();
    assert_eq!(err.code, "bad-pattern");
    assert!(
        !ferry_cli::folder::state_dir(&proj)
            .join("pin-state.json")
            .exists(),
        "a refused start must not leave a marker behind"
    );
}

#[test]
fn stale_pin_surfaces_then_a_new_start_replaces_it() {
    let (_e, proj, _daemon) = setup();

    // Orphaned writer: kill a child, keep its pid.
    let mut child = ferry_platform::spawn_sleeper(30).unwrap();
    let dead_pid = {
        child.kill().unwrap();
        child.wait().unwrap();
        child.id()
    };

    let store = ferry_pin::PinStore::new(ferry_cli::folder::state_dir(&proj));
    store
        .start(&ferry_pin::PinRecord {
            format_version: ferry_pin::PIN_FORMAT_VERSION,
            device_id: "a".repeat(32),
            pid: dead_pid,
            started_sec: 1_787_574_000,
            started_nsec: 0,
            expires_sec: None,
            paths: vec!["*".into()],
            released: false,
            base_agreements: BTreeMap::new(),
            proc_start_token: None,
        })
        .unwrap();

    // Surfaced as stale — visible, holding nothing, never silently dropped.
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "stale");
    assert_eq!(st.json["holding"], false);

    // Replacement is the recovery path and must not hit pin-active.
    commands::pin::start(&proj, &["src/**".to_string()], 8).unwrap();
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "active");
}

#[test]
fn status_command_shows_the_held_set_too() {
    let (_e, proj, _daemon) = setup();
    commands::pin::start(&proj, &["src/**".to_string()], 8).unwrap();
    let peer = "c".repeat(32);
    hold_one_real_change(&proj, &peer, "src/a.rs");

    let out = commands::status::run(&proj).unwrap();
    assert_eq!(out.json["pin"]["state"], "active");
    assert_eq!(out.json["held_changes"], 1);
    assert_eq!(out.json["held_by_peer"][&peer][0], "src/a.rs");
}

#[test]
fn pin_start_fails_when_daemon_not_running() {
    let env = Env::new("pin-no-daemon");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj).unwrap();

    let err = commands::pin::start(&proj, &[], 8).unwrap_err();
    assert_eq!(err.code, "daemon-not-running");
    assert!(err.message.contains("no active background daemon"));
    assert!(err.hint.contains("ferry daemon"));
}
