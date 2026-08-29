//! Integration tests for three-way reconciliation and unpinned conflict quarantine.
//!
//! Acceptance Criteria:
//! 1. Core sync exchange invokes three-way reconciliation against last-agreed manifest base.
//! 2. Concurrent edits to the same file between unpinned devices preserve the winner in place
//!    and write the losing file to `<path>.ferry-conflict.<loser-device>-<timestamp>`.
//! 3. Every quarantined conflict generates an immutable entry in persistent conflict report
//!    ledger (`conflicts.jsonl`).
//! 4. Simultaneous edit-versus-delete conflicts resurrect the edited file.
//! 5. Identical content modifications differing only in timestamps or executable permissions
//!    resolve deterministically without creating duplicate conflict files.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ferry_sync::format::hex;
use ferry_sync::{engine, EngineConfig, EngineHandle, SyncEngine};
use ferry_sync_engine::report::ConflictEntry;

use common::{default_transport, timeout_from_env, EngineFixture};

const SEED: u64 = 99;

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout_from_env();
    while !cond() {
        assert!(Instant::now() <= deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_to_string(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

fn write_with_mtime(path: &Path, content: &[u8], sec: i64, nsec: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(sec, nsec)).unwrap();
}

fn restart_engines(fx: &EngineFixture, tag_a: &str, tag_b: &str) -> (EngineHandle, EngineHandle) {
    let mut cfg_a = EngineConfig::default_for_test(fx.poly);
    cfg_a.tag = tag_a.to_string();
    cfg_a.store_dir = fx._dir.path().join("a/store");
    cfg_a.tree_dir = fx._dir.path().join("a/tree");
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let engine_a = SyncEngine::new(cfg_a, default_transport()).expect("engine A restart");
    let addr = engine_a
        .listen_addr()
        .expect("A must report its bound port");
    let handle_a = engine_a.start();

    let mut cfg_b = EngineConfig::default_for_test(fx.poly);
    cfg_b.tag = tag_b.to_string();
    cfg_b.store_dir = fx._dir.path().join("b/store");
    cfg_b.tree_dir = fx._dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr);
    let engine_b = SyncEngine::new(cfg_b, default_transport()).expect("engine B restart");
    let handle_b = engine_b.start();

    (handle_a, handle_b)
}

fn list_conflict_files(tree: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(tree) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.contains(".ferry-conflict.") {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

fn read_conflict_log(store_dir: &Path) -> Vec<ConflictEntry> {
    let dot_ferry = store_dir.join(".ferry");
    let dir = if dot_ferry.is_dir() {
        &dot_ferry
    } else {
        store_dir
    };
    ferry_sync_engine::report::list_conflicts(dir).unwrap_or_default()
}

#[test]
fn unpinned_concurrent_edits_preserve_winner_quarantine_loser_and_log_conflict() {
    let name = "unpinned-quarantine";
    let tag_a = format!("{name}-a");
    let tag_b = format!("{name}-b");
    let dev_a = *engine::device_identity_for_tag(&tag_a).device_id();
    let dev_b = *engine::device_identity_for_tag(&tag_b).device_id();
    let hex_a = hex(&dev_a);
    let hex_b = hex(&dev_b);

    let fx = EngineFixture::start(name, SEED);
    let store_a = fx._dir.path().join("a/store");
    let store_b = fx._dir.path().join("b/store");

    // Phase 1: Converge baseline file notes.txt
    write_with_mtime(
        &fx.tree_a().join("notes.txt"),
        b"initial baseline notes",
        1_700_000_000,
        0,
    );
    wait_until(
        "baseline notes.txt converges on B and agreement recorded",
        || {
            fx.converged()
                && read_to_string(&fx.tree_b(), "notes.txt").as_deref()
                    == Some("initial baseline notes")
        },
    );

    // Phase 2: Stop engines and perform concurrent unpinned modifications offline
    fx.b.shutdown();
    fx.a.shutdown();

    // Node A edits notes.txt with newer mtime (winner)
    let winner_mtime = 1_800_000_200i64;
    write_with_mtime(
        &fx.tree_a().join("notes.txt"),
        b"version A winning content\n",
        winner_mtime,
        0,
    );

    // Node B edits notes.txt with older mtime (loser)
    let loser_mtime = 1_800_000_100i64;
    write_with_mtime(
        &fx.tree_b().join("notes.txt"),
        b"version B losing content\n",
        loser_mtime,
        0,
    );

    // Phase 3: Restart engines to sync over loopback TCP
    let (handle_a, handle_b) = restart_engines(&fx, &tag_a, &tag_b);

    // Winner file must stay live at notes.txt on both nodes
    wait_until("winner content is live on both trees", || {
        read_to_string(&fx.tree_a(), "notes.txt").as_deref() == Some("version A winning content\n")
            && read_to_string(&fx.tree_b(), "notes.txt").as_deref()
                == Some("version A winning content\n")
    });

    // Expected quarantine filename for loser (device B):
    // <name>.ferry-conflict.<loser_short_hex>-<loser_mtime_compact>
    let loser_short_hex = &hex_b[..8];
    let loser_ts_compact = ferry_platform::time::fmt_compact(loser_mtime);
    let expected_conflict_name =
        format!("notes.txt.ferry-conflict.{loser_short_hex}-{loser_ts_compact}");

    // Wait until quarantine file is present and contains loser content on both sides
    wait_until("loser quarantine file is present on A", || {
        read_to_string(&fx.tree_a(), &expected_conflict_name).as_deref()
            == Some("version B losing content\n")
    });
    wait_until("loser quarantine file is present on B", || {
        read_to_string(&fx.tree_b(), &expected_conflict_name).as_deref()
            == Some("version B losing content\n")
    });

    // Phase 4: Verify conflicts.jsonl on both nodes
    wait_until("both_changed conflict is logged", || {
        let entries_a = read_conflict_log(&store_a);
        let entries_b = read_conflict_log(&store_b);
        entries_a
            .iter()
            .chain(entries_b.iter())
            .any(|e| e.path == "notes.txt")
    });

    let entries_a = read_conflict_log(&store_a);
    let entries_b = read_conflict_log(&store_b);
    let c = entries_a
        .iter()
        .chain(entries_b.iter())
        .find(|e| e.path == "notes.txt")
        .expect("conflict record for notes.txt");
    assert_eq!(c.path, "notes.txt");
    assert_eq!(c.kind, "both_changed");
    assert_eq!(c.winner.device, hex_a);
    assert_eq!(c.winner.mtime_sec, Some(winner_mtime));
    assert_eq!(c.loser.device, hex_b);
    assert_eq!(c.loser.mtime_sec, Some(loser_mtime));
    assert_eq!(
        c.quarantined_as.as_deref(),
        Some(expected_conflict_name.as_str())
    );

    handle_b.shutdown();
    handle_a.shutdown();
}

#[test]
fn edit_versus_delete_resurrects_edited_file() {
    let name = "edit-vs-delete";
    let tag_a = format!("{name}-a");
    let tag_b = format!("{name}-b");
    let dev_a = *engine::device_identity_for_tag(&tag_a).device_id();
    let dev_b = *engine::device_identity_for_tag(&tag_b).device_id();
    let hex_a = hex(&dev_a);
    let hex_b = hex(&dev_b);

    let fx = EngineFixture::start(name, SEED + 1);
    let store_a = fx._dir.path().join("a/store");
    let store_b = fx._dir.path().join("b/store");

    // Phase 1: Baseline files keep.txt and file.txt
    write_with_mtime(
        &fx.tree_a().join("keep.txt"),
        b"keep this file",
        1_700_000_000,
        0,
    );
    write_with_mtime(
        &fx.tree_a().join("file.txt"),
        b"baseline text to be preserved",
        1_700_000_000,
        0,
    );
    wait_until(
        "baseline files converge on B and agreement recorded",
        || {
            fx.converged()
                && read_to_string(&fx.tree_b(), "file.txt").as_deref()
                    == Some("baseline text to be preserved")
        },
    );

    // Phase 2: Offline - Node A edits file.txt, Node B deletes file.txt
    fx.b.shutdown();
    fx.a.shutdown();

    let edit_mtime = 1_800_000_300i64;
    write_with_mtime(
        &fx.tree_a().join("file.txt"),
        b"resurrected content edited on A\n",
        edit_mtime,
        0,
    );
    std::fs::remove_file(fx.tree_b().join("file.txt")).unwrap();

    // Phase 3: Resume sync
    let (handle_a, handle_b) = restart_engines(&fx, &tag_a, &tag_b);

    // Edited file must resurrect and be present on both sides
    wait_until("resurrected edit lands on both trees", || {
        read_to_string(&fx.tree_a(), "file.txt").as_deref()
            == Some("resurrected content edited on A\n")
            && read_to_string(&fx.tree_b(), "file.txt").as_deref()
                == Some("resurrected content edited on A\n")
    });

    // No quarantine copy since deletion has no content to quarantine
    assert!(
        list_conflict_files(&fx.tree_a()).is_empty(),
        "no quarantine file should be generated for delete-vs-edit"
    );
    assert!(
        list_conflict_files(&fx.tree_b()).is_empty(),
        "no quarantine file should be generated for delete-vs-edit"
    );

    // Check conflict log entries
    wait_until("delete_vs_edit conflict is logged", || {
        let entries_a = read_conflict_log(&store_a);
        let entries_b = read_conflict_log(&store_b);
        entries_a
            .iter()
            .chain(entries_b.iter())
            .any(|e| e.path == "file.txt")
    });

    let entries_a = read_conflict_log(&store_a);
    let entries_b = read_conflict_log(&store_b);
    let c = entries_a
        .iter()
        .chain(entries_b.iter())
        .find(|e| e.path == "file.txt")
        .expect("conflict record for delete_vs_edit on file.txt");
    assert_eq!(c.kind, "delete_vs_edit");
    assert_eq!(c.winner.device, hex_a);
    assert_eq!(c.loser.device, hex_b);
    assert_eq!(c.loser.mtime_sec, None, "deleted loser has no mtime");
    assert_eq!(c.quarantined_as, None, "no quarantine file for deletion");

    handle_b.shutdown();
    handle_a.shutdown();
}

#[test]
fn identical_content_edits_differing_mtimes_resolve_silently_without_conflict_file() {
    let name = "identical-edits";
    let tag_a = format!("{name}-a");
    let tag_b = format!("{name}-b");
    let _dev_a = *engine::device_identity_for_tag(&tag_a).device_id();
    let _dev_b = *engine::device_identity_for_tag(&tag_b).device_id();

    let fx = EngineFixture::start(name, SEED + 2);
    let store_a = fx._dir.path().join("a/store");
    let store_b = fx._dir.path().join("b/store");

    // Phase 1: Baseline file doc.txt
    write_with_mtime(
        &fx.tree_a().join("doc.txt"),
        b"initial baseline doc",
        1_700_000_000,
        0,
    );
    wait_until(
        "baseline doc.txt converges on B and agreement recorded",
        || {
            fx.converged()
                && read_to_string(&fx.tree_b(), "doc.txt").as_deref()
                    == Some("initial baseline doc")
        },
    );

    // Phase 2: Offline - Both nodes write identical content with different timestamps
    fx.b.shutdown();
    fx.a.shutdown();

    let identical_bytes = b"exact same modified bytes on both nodes\n";
    write_with_mtime(
        &fx.tree_a().join("doc.txt"),
        identical_bytes,
        1_800_000_500,
        0,
    );
    write_with_mtime(
        &fx.tree_b().join("doc.txt"),
        identical_bytes,
        1_800_000_400,
        0,
    );

    // Phase 3: Resume sync
    let (handle_a, handle_b) = restart_engines(&fx, &tag_a, &tag_b);

    // Both converge on identical bytes
    wait_until("both trees hold identical modified content", || {
        read_to_string(&fx.tree_a(), "doc.txt").as_deref()
            == Some("exact same modified bytes on both nodes\n")
            && read_to_string(&fx.tree_b(), "doc.txt").as_deref()
                == Some("exact same modified bytes on both nodes\n")
    });

    // No conflict files must be created
    assert!(
        list_conflict_files(&fx.tree_a()).is_empty(),
        "identical content must not produce conflict quarantine file on A"
    );
    assert!(
        list_conflict_files(&fx.tree_b()).is_empty(),
        "identical content must not produce conflict quarantine file on B"
    );

    // Conflict log must not contain an entry for doc.txt
    let entries_a = read_conflict_log(&store_a);
    let entries_b = read_conflict_log(&store_b);
    assert!(
        !entries_a.iter().any(|e| e.path == "doc.txt"),
        "doc.txt must not be logged as a conflict on A"
    );
    assert!(
        !entries_b.iter().any(|e| e.path == "doc.txt"),
        "doc.txt must not be logged as a conflict on B"
    );

    handle_b.shutdown();
    handle_a.shutdown();
}

#[cfg(unix)]
#[test]
fn identical_content_differing_permissions_resolves_silently() {
    use std::os::unix::fs::PermissionsExt;

    let name = "identical-exec";
    let tag_a = format!("{name}-a");
    let tag_b = format!("{name}-b");
    let _dev_b = *engine::device_identity_for_tag(&tag_b).device_id();

    let fx = EngineFixture::start(name, SEED + 3);
    let _store_a = fx._dir.path().join("a/store");

    // Phase 1: Baseline file script.sh
    let script_bytes = b"#!/bin/sh\necho hello\n";
    write_with_mtime(
        &fx.tree_a().join("script.sh"),
        script_bytes,
        1_700_000_000,
        0,
    );
    wait_until(
        "baseline script.sh converges on B and agreement recorded",
        || {
            fx.converged()
                && read_to_string(&fx.tree_b(), "script.sh").as_deref()
                    == Some("#!/bin/sh\necho hello\n")
        },
    );

    // Phase 2: Offline - Node A sets executable bit, Node B does not (same content)
    fx.b.shutdown();
    fx.a.shutdown();

    let p_a = fx.tree_a().join("script.sh");
    let p_b = fx.tree_b().join("script.sh");
    write_with_mtime(&p_a, script_bytes, 1_800_000_600, 0);
    let mut perm = std::fs::metadata(&p_a).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&p_a, perm).unwrap();

    write_with_mtime(&p_b, script_bytes, 1_800_000_500, 0);
    let mut perm_b = std::fs::metadata(&p_b).unwrap().permissions();
    perm_b.set_mode(0o644);
    std::fs::set_permissions(&p_b, perm_b).unwrap();

    // Phase 3: Resume sync
    let (handle_a, handle_b) = restart_engines(&fx, &tag_a, &tag_b);

    // Wait until both converge
    wait_until("both trees hold identical script", || {
        read_to_string(&fx.tree_a(), "script.sh").as_deref() == Some("#!/bin/sh\necho hello\n")
            && read_to_string(&fx.tree_b(), "script.sh").as_deref()
                == Some("#!/bin/sh\necho hello\n")
    });

    // No conflict quarantine files
    assert!(
        list_conflict_files(&fx.tree_a()).is_empty(),
        "permission-only divergence must not create conflict files on A"
    );
    assert!(
        list_conflict_files(&fx.tree_b()).is_empty(),
        "permission-only divergence must not create conflict files on B"
    );

    handle_b.shutdown();
    handle_a.shutdown();
}
