//! `--json` schema stability: every command's document is reduced to its
//! KEY STRUCTURE (sorted path:type pairs, array shapes pinned by element)
//! and compared against checked-in files under tests/expected/. Values are
//! deliberately ignored — these snapshots pin NAMES and TYPES, which is the
//! stability promise docs/cli-json.md makes.

mod common;

use common::{Env, RunningDaemon};
use ferry_cli::commands;
use serde_json::Value;

/// Reduce a JSON value to a deterministic schema description.
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
            // Pin the shape of the FIRST element only (arrays are
            // homogeneous by contract).
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

/// Compare-or-bless. Set `FERRY_UPDATE_EXPECTED=1` to rewrite the file.
fn assert_matches_expected(name: &str, actual: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/expected");
    let file = dir.join(format!("{name}.schema.txt"));
    if std::env::var("FERRY_UPDATE_EXPECTED").is_ok() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, actual).unwrap();
        eprintln!("blessed {}", file.display());
        return;
    }
    let expected = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "missing expected schema {} — run with FERRY_UPDATE_EXPECTED=1 to bless",
            file.display()
        )
    });
    // Normalize checkout line endings: git may hand these fixtures back with
    // CRLF on windows runners (autocrlf), while generated schemas always use
    // LF. The schema bytes are what this test guards, not the newline style.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(expected, actual, "JSON schema for {name} drifted");
}

#[test]
fn init_document_schema_is_stable() {
    let env = Env::new("schema-init");
    let proj = env.work().join("proj");
    let out = commands::init::run(&proj, "init").unwrap();
    assert_matches_expected("init", &schema_of(&out.json));
}

#[test]
fn status_document_schema_is_stable() {
    let env = Env::new("schema-status");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();
    std::fs::write(proj.join("a.txt"), b"x").unwrap();
    let out = commands::status::run(&proj).unwrap();
    assert_matches_expected("status", &schema_of(&out.json));
}

#[test]
fn conflicts_document_schema_is_stable() {
    let env = Env::new("schema-conflicts");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();
    ferry_sync_engine::append_entries(
        &ferry_cli::folder::state_dir(&proj),
        &[ferry_sync_engine::ConflictEntry {
            ts: "2026-08-24T10:00:00Z".into(),
            folder_id: "aa".repeat(16),
            path: "f.txt".into(),
            kind: "both_changed".into(),
            winner: ferry_sync_engine::DeviceStamp {
                device: "bb".repeat(32),
                mtime_sec: Some(2),
                mtime_nsec: Some(0),
            },
            loser: ferry_sync_engine::DeviceStamp {
                device: "cc".repeat(32),
                mtime_sec: Some(1),
                mtime_nsec: Some(0),
            },
            quarantined_as: Some("f.txt.ferry-conflict.cccccccc-20260824-090000".into()),
        }],
    )
    .unwrap();
    let out = commands::conflicts::run(&proj).unwrap();
    assert_matches_expected("conflicts", &schema_of(&out.json));
}

#[test]
fn ignore_list_document_schema_is_stable() {
    let env = Env::new("schema-ignore");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();
    commands::ignore_cmd::run(&proj, Some("*.log"), None, false).unwrap();
    commands::ignore_cmd::run(&proj, None, Some("claude"), false).unwrap();
    let out = commands::ignore_cmd::run(&proj, None, None, true).unwrap();
    assert_matches_expected("ignore-list", &schema_of(&out.json));
}

#[test]
fn ignore_mutations_share_one_document_shape() {
    let env = Env::new("schema-ignore-mutate");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();
    let added = commands::ignore_cmd::run(&proj, Some("dist/"), None, false).unwrap();
    assert_matches_expected("ignore-added", &schema_of(&added.json));
    let preset = commands::ignore_cmd::run(&proj, None, Some("opencode"), false).unwrap();
    assert_matches_expected("ignore-preset", &schema_of(&preset.json));
}

#[test]
fn store_gc_document_schema_is_stable() {
    let env = Env::new("schema-store-gc");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();

    // Orphan content: blobs with no manifest referencing them must show up
    // as garbage in the dry-run report (and pin the array element shape).
    let opened = ferry_cli::folder::open_folder(&proj).unwrap();
    opened
        .store
        .put_data(b"orphan bytes nobody references")
        .unwrap();
    opened.store.flush().unwrap();
    opened.store.write_index_snapshot().unwrap();

    let dry = commands::store::run(commands::store::GcArgs {
        folder: &proj,
        dry_run: true,
        grace_secs: 24 * 60 * 60,
    })
    .unwrap();
    assert_eq!(dry.json["command"], "store");
    assert_eq!(dry.json["dry_run"], true);
    assert!(!dry.json["garbage_packs"].as_array().unwrap().is_empty());
    assert_matches_expected("store-gc-dry", &schema_of(&dry.json));

    // The delete path behind the report shares the document skeleton.
    let real = commands::store::run(commands::store::GcArgs {
        folder: &proj,
        dry_run: false,
        grace_secs: 0,
    })
    .unwrap();
    assert_eq!(real.json["dry_run"], false);
    assert_matches_expected("store-gc", &schema_of(&real.json));
}

#[test]
fn pin_documents_are_stable_across_the_lifecycle() {
    let env = Env::new("schema-pin");
    let proj = env.work().join("proj");
    commands::init::run(&proj, "init").unwrap();
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/a.rs"), b"fn main() {}\n").unwrap();

    let _daemon = RunningDaemon::start(&proj);
    let started = commands::pin::start(&proj, &["src/**".to_string()]).unwrap();
    assert_matches_expected("pin-start", &schema_of(&started.json));

    // While pinned with held changes absent, status still pins the shape.
    let status = commands::pin::status(&proj).unwrap();
    assert_eq!(status.json["state"], "active");

    // Simulate a held change arriving (the exchange loop writes these);
    // its manifest must REALLY be in the store or release refuses loudly.
    let opened = ferry_cli::folder::open_folder(&proj).unwrap();
    let scan = ferry_cli::commands::status::scan_now(&opened).unwrap();
    let mid = opened
        .store
        .put_meta(
            ferry_store::format::BlobKind::Manifest,
            &scan.manifest_bytes,
        )
        .unwrap();
    // Staged metadata is only readable after sealing (same rule the
    // exchange loop follows before anything references it).
    opened.store.flush().unwrap();
    opened.store.write_index_snapshot().unwrap();
    let state_dir = ferry_cli::folder::state_dir(&proj);
    let ledger = ferry_pin::HeldLedger::new(&state_dir);
    ledger
        .append(
            &"b".repeat(32),
            &[ferry_pin::HeldEntry {
                held_sec: 1_787_574_000,
                held_nsec: 0,
                path: "src/a.rs".into(),
                device_id: "b".repeat(32),
                remote_manifest_id: ferry_store::format::hex(&mid),
                chunks: Vec::new(),
                decision: "remote_apply".into(),
                conflict_winner: None,
            }],
        )
        .unwrap();

    let status_held = commands::pin::status(&proj).unwrap();
    assert_matches_expected("pin-status", &schema_of(&status_held.json));
    assert_eq!(status_held.json["held_changes"], 1);

    let stopped = commands::pin::stop(&proj).unwrap();
    assert_matches_expected("pin-stop", &schema_of(&stopped.json));

    let released = commands::pin::release(&proj).unwrap();
    assert_matches_expected("pin-release", &schema_of(&released.json));
}
