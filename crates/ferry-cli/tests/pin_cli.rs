mod common;

use common::{Env, RunningDaemon};
use ferry_cli::commands;
use ferry_store::format::BlobKind;
use ferry_sync_engine::pin::{HeldEntry, HeldLedger};
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

    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["command"], "pin");
    assert_eq!(st.json["action"], "status");
    assert_eq!(st.json["state"], "none");
    assert_eq!(st.json["holding"], false);
    assert_eq!(st.json["held_changes"], 0);

    let out = commands::pin::start(&proj, &["src/**".to_string()], 8).unwrap();
    assert_eq!(out.json["action"], "start");
    assert_eq!(out.json["paths"][0], "src/**");
    assert!(out.json["pid"].as_u64().is_some());
    let device = out.json["device_id"].as_str().unwrap().to_string();
    assert_eq!(device.len(), 64);

    let err = commands::pin::start(&proj, &[], 8).unwrap_err();
    assert_eq!(err.code, "pin-active");

    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "active");
    assert_eq!(st.json["holding"], true);

    let peer = "b".repeat(32);
    hold_one_real_change(&proj, &peer, "src/a.rs");
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["held_changes"], 1);
    assert_eq!(
        st.json["held_by_peer"][&peer][0].as_str().unwrap(),
        "src/a.rs"
    );

    let stopped = commands::pin::stop(&proj).unwrap();
    assert_eq!(stopped.json["was_pinned"], true);
    assert_eq!(stopped.json["held_changes"], 1);
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "released");

    let rel = commands::pin::release(&proj).unwrap();
    assert_eq!(rel.json["action"], "release");
    let peers = rel.json["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["device_id"], peer.as_str());
    assert_eq!(peers[0]["held_paths"][0], "src/a.rs");
    assert_eq!(rel.json["pin_ended"], true);

    let again = commands::pin::release(&proj).unwrap();
    assert_eq!(again.json["peers"].as_array().unwrap().len(), 0);
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["held_changes"], 0);

    commands::pin::start(&proj, &[], 8).unwrap();
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

    let mut child = ferry_platform::spawn_sleeper(30).unwrap();
    let dead_pid = {
        child.kill().unwrap();
        child.wait().unwrap();
        child.id()
    };

    let store = ferry_sync_engine::pin::PinStore::new(ferry_cli::folder::state_dir(&proj));
    store
        .start(&ferry_sync_engine::pin::PinRecord {
            format_version: ferry_sync_engine::pin::PIN_FORMAT_VERSION,
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

    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "stale");
    assert_eq!(st.json["holding"], false);

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
fn pin_start_autostarts_daemon_when_not_running() {
    let env = Env::new("pin-no-daemon");
    let proj = env.work().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj).unwrap();

    let out = commands::pin::start(&proj, &[], 8).unwrap();
    assert_eq!(out.json["action"], "start");
    assert_eq!(out.json["command"], "pin");
}

fn write_file_with_mtime(path: &std::path::Path, bytes: &[u8], mtime_sec: u64) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime_sec)),
    )
    .unwrap();
}

fn transfer_snapshot(
    from: &ferry_store::store::Store,
    to: &ferry_store::store::Store,
    snap: &ferry_store::snapshot::SnapshotOutput,
) {
    if to.get(BlobKind::Manifest, &snap.manifest_id).is_err() {
        let b = from.get(BlobKind::Manifest, &snap.manifest_id).unwrap();
        to.put_blob(BlobKind::Manifest, &b).unwrap();
    }
    let mut stack = vec![snap.root_tree_id];
    while let Some(id) = stack.pop() {
        if to.get(BlobKind::TreeNode, &id).is_ok() {
            continue;
        }
        let b = from.get(BlobKind::TreeNode, &id).unwrap();
        to.put_blob(BlobKind::TreeNode, &b).unwrap();
        let node = ferry_store::manifest::parse_tree_node(&b).unwrap();
        for e in node.entries {
            match e.payload {
                ferry_store::manifest::EntryPayload::Dir { child_tree_id } => {
                    stack.push(child_tree_id);
                }
                ferry_store::manifest::EntryPayload::File { chunks, .. } => {
                    for (cid, _) in chunks {
                        if to.get(BlobKind::DataChunk, &cid).is_err() {
                            let cb = from.get(BlobKind::DataChunk, &cid).unwrap();
                            to.put_blob(BlobKind::DataChunk, &cb).unwrap();
                        }
                    }
                }
                ferry_store::manifest::EntryPayload::Symlink { .. } => {}
            }
        }
    }
    to.flush().unwrap();
    to.write_index_snapshot().unwrap();
}

#[test]
fn pin_release_applies_nonconflicting_and_quarantines_conflicting_held_edits() {
    use ferry_store::agreement::{AgreedRecord, AgreementLedger};
    use ferry_store::format::hex;
    use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};

    let (env, proj, _daemon) = setup();
    let dev_b: [u8; 32] = [0xB2; 32];
    let dev_b_hex = hex(&dev_b);

    // Initial files with fixed mtime 1000
    write_file_with_mtime(&proj.join("src/a.rs"), b"fn main() {}\n", 1000);
    write_file_with_mtime(&proj.join("README.md"), b"hi\n", 1000);
    write_file_with_mtime(&proj.join("notes.txt"), b"notes-v0", 1000);
    write_file_with_mtime(&proj.join("doc.txt"), b"doc-v0", 1000);

    let opened = ferry_cli::folder::open_folder(&proj).unwrap();
    let poly = ferry_store::chunker::ValidatedPoly::new(opened.poly).unwrap();
    let base_snap = snapshot_dir(
        &opened.store,
        poly,
        &proj,
        &SnapshotIdentity {
            folder_id: opened.folder_id,
            device_id: [0xA1; 32],
            parent_manifest_id: [0; 32],
            created_sec: 1000,
            created_nsec: 0,
        },
    )
    .unwrap();
    opened.store.flush().unwrap();
    opened.store.write_index_snapshot().unwrap();

    // Record baseline agreement with peer B
    AgreementLedger::new(opened.state_dir())
        .record(
            &opened.folder_id,
            &AgreedRecord {
                peer_device_id: dev_b,
                manifest_id: base_snap.manifest_id,
                agreed_sec: 1000,
                agreed_nsec: 0,
            },
        )
        .unwrap();

    // Start pin
    commands::pin::start(&proj, &["*".to_string()], 8).unwrap();
    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["state"], "active");
    assert_eq!(st.json["holding"], true);

    // Local edits notes.txt at mtime 2000 (later than remote)
    write_file_with_mtime(&proj.join("notes.txt"), b"notes-v1-local", 2000);

    // Simulate remote peer B: creates remote tree and store
    let remote_temp = env.work().join("peer_b");
    std::fs::create_dir_all(remote_temp.join("src")).unwrap();
    write_file_with_mtime(&remote_temp.join("src/a.rs"), b"fn main() {}\n", 1000);
    write_file_with_mtime(&remote_temp.join("README.md"), b"hi\n", 1000);
    let remote_store_dir = env.work().join("peer_b_store");
    std::fs::create_dir_all(&remote_store_dir).unwrap();
    let remote_store = ferry_store::store::Store::create(
        &remote_store_dir,
        core::array::from_fn(|i| (i * 17 + 3) as u8),
        Box::new(ferry_store::crypto::PassthroughCipher),
    )
    .unwrap();

    // Remote peer B edits notes.txt (mtime 1500) and doc.txt (mtime 1500)
    write_file_with_mtime(&remote_temp.join("notes.txt"), b"notes-v1-remote", 1500);
    write_file_with_mtime(&remote_temp.join("doc.txt"), b"doc-v1-remote", 1500);

    let remote_snap = snapshot_dir(
        &remote_store,
        poly,
        &remote_temp,
        &SnapshotIdentity {
            folder_id: opened.folder_id,
            device_id: dev_b,
            parent_manifest_id: base_snap.manifest_id,
            created_sec: 1500,
            created_nsec: 0,
        },
    )
    .unwrap();
    remote_store.flush().unwrap();
    remote_store.write_index_snapshot().unwrap();

    // Transfer remote blobs into local store (simulating sync exchange hold)
    transfer_snapshot(&remote_store, &opened.store, &remote_snap);

    // Ledger the held entries
    let ledger = HeldLedger::new(opened.state_dir());
    ledger
        .append(
            &dev_b_hex,
            &[
                HeldEntry {
                    held_sec: 1500,
                    held_nsec: 0,
                    path: "notes.txt".into(),
                    device_id: dev_b_hex.clone(),
                    remote_manifest_id: hex(&remote_snap.manifest_id),
                    chunks: vec![],
                    decision: "conflict".into(),
                    conflict_winner: Some("local".into()),
                },
                HeldEntry {
                    held_sec: 1500,
                    held_nsec: 0,
                    path: "doc.txt".into(),
                    device_id: dev_b_hex.clone(),
                    remote_manifest_id: hex(&remote_snap.manifest_id),
                    chunks: vec![],
                    decision: "remote_apply".into(),
                    conflict_winner: None,
                },
            ],
        )
        .unwrap();

    let st = commands::pin::status(&proj).unwrap();
    assert_eq!(st.json["held_changes"], 2);

    // Run pin release
    let rel = commands::pin::release(&proj).unwrap();
    eprintln!(
        "DEBUG rel.json: {}",
        serde_json::to_string_pretty(&rel.json).unwrap()
    );
    let cf = ferry_sync_engine::list_conflicts(&ferry_cli::folder::state_dir(&proj)).unwrap();
    eprintln!("DEBUG conflicts in log: {cf:#?}");
    assert_eq!(rel.json["command"], "pin");
    assert_eq!(rel.json["action"], "release");
    assert_eq!(rel.json["pin_ended"], true);
    assert_eq!(rel.json["quarantined"], 1);
    assert_eq!(rel.json["conflicts_recorded"], 1);
    assert!(rel.json["ops_applied"].as_u64().unwrap() >= 1);

    // Verify non-conflicting change applied to working tree
    assert_eq!(
        std::fs::read_to_string(proj.join("doc.txt")).unwrap(),
        "doc-v1-remote"
    );

    // Verify conflicting change kept local winner in working tree
    assert_eq!(
        std::fs::read_to_string(proj.join("notes.txt")).unwrap(),
        "notes-v1-local"
    );

    // Verify quarantined conflict file exists with format <file>.ferry-conflict.<device>-<timestamp>
    let mut conflict_files = Vec::new();
    for entry in std::fs::read_dir(&proj).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(".ferry-conflict.") {
            conflict_files.push(name);
        }
    }
    assert_eq!(
        conflict_files.len(),
        1,
        "expected 1 conflict file: {conflict_files:?}"
    );
    assert!(conflict_files[0].starts_with("notes.txt.ferry-conflict."));
    assert_eq!(
        std::fs::read_to_string(proj.join(&conflict_files[0])).unwrap(),
        "notes-v1-remote"
    );

    // Verify conflict entry in .ferry/conflicts.jsonl
    let conflicts =
        ferry_sync_engine::list_conflicts(&ferry_cli::folder::state_dir(&proj)).unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "notes.txt");
    assert_eq!(conflicts[0].loser.device, dev_b_hex);

    // Verify held ledger is cleaned up
    let st_after = commands::pin::status(&proj).unwrap();
    assert_eq!(st_after.json["holding"], false);
    assert_eq!(st_after.json["held_changes"], 0);
}
