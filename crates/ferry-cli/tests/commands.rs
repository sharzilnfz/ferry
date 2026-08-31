use std::path::Path;

use ferry_cli::commands;
use ferry_cli::folder::{self, Settings, SETTINGS_FORMAT_VERSION};

struct Env {
    _home: tempfile::TempDir,
    _work: tempfile::TempDir,

    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn setup() -> (Env, std::path::PathBuf, std::path::PathBuf) {
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::env::set_var("FERRY_HOME", home.path());
    let env = Env {
        _home: home,
        _work: work,
        _guard: guard,
    };
    let hp = env._home.path().to_path_buf();
    let wp = env._work.path().to_path_buf();
    (env, hp, wp)
}

#[test]
fn init_creates_store_config_settings_and_ignore_file() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    let out = commands::init::run(&proj).expect("init succeeds");
    assert_eq!(out.json["command"], "init");
    assert_eq!(out.json["created"], true);
    let folder_id = out.json["folder_id"].as_str().unwrap();
    assert_eq!(folder_id.len(), 32);

    assert!(proj.join(".ferry/config").is_file());
    assert!(proj.join(".ferry/packs").is_dir());
    assert!(proj.join(".ferry/index").is_dir());
    assert!(proj.join(".ferry/settings.json").is_file());

    assert!(proj.join("ferry.ignore").is_file());
    assert!(out.human.contains("ferry pair"));

    let head = ferry_crypto::config_head::parse_config_head(
        &std::fs::read(proj.join(".ferry/config")).unwrap(),
    )
    .unwrap();
    assert_eq!(head.entries.len(), 1);
    assert_eq!(ferry_store::format::hex(&head.folder_id), folder_id);
}

#[test]
fn init_twice_is_a_friendly_error() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    commands::init::run(&proj).unwrap();
    let err = commands::init::run(&proj).unwrap_err();
    assert_eq!(err.code, "already-initialized");
}

#[test]
fn init_creates_second_folder_under_same_identity() {
    let (_e, home, work) = setup();
    let other = work.join("other");
    let out = commands::init::run(&other).unwrap();
    assert_eq!(out.json["command"], "init");

    assert!(home.join("identity/device.key").is_file());
}

#[test]
fn status_reports_scan_manifest_and_empty_peers_conflicts() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    std::fs::write(work.join("seed.txt"), b"hello ferry").unwrap();
    std::fs::rename(work.join("seed.txt"), proj.join("a.txt")).unwrap_or_else(|_| {
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("a.txt"), b"hello ferry").unwrap();
    });

    commands::init::run(&proj).unwrap();
    let out = commands::status::run(&proj).unwrap();
    let doc = &out.json;

    assert_eq!(doc["command"], "status");
    assert_eq!(doc["manifest_id"].as_str().unwrap().len(), 64);

    assert_eq!(doc["scanned"]["files"], 2);
    assert!(doc["pending_changes"].is_null(), "no agreement yet");
    assert_eq!(doc["conflicts"], 0);
    let peers = doc["peers"].as_array().unwrap();
    assert!(peers.is_empty());
    assert!(out.human.contains("none yet"));
}

#[test]
fn conflicts_list_reads_jsonl_written_behind_it() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    commands::init::run(&proj).unwrap();

    let entry = ferry_sync_engine::ConflictEntry {
        ts: "2026-08-24T10:00:00Z".into(),
        folder_id: "aa".repeat(16),
        path: "notes.txt".into(),
        kind: "both_changed".into(),
        winner: ferry_sync_engine::DeviceStamp {
            device: "bb".repeat(32),
            mtime_sec: Some(100),
            mtime_nsec: Some(5),
        },
        loser: ferry_sync_engine::DeviceStamp {
            device: "cc".repeat(32),
            mtime_sec: Some(90),
            mtime_nsec: Some(5),
        },
        quarantined_as: Some("notes.txt.ferry-conflict.cccccccc-20260824-090000".into()),
    };
    ferry_sync_engine::append_entries(&folder::state_dir(&proj), std::slice::from_ref(&entry))
        .unwrap();

    let out = commands::conflicts::run(&proj).unwrap();
    assert_eq!(out.json["entries"][0]["path"], "notes.txt");
    assert_eq!(out.json["entries"][0]["kind"], "both_changed");
    assert!(out.human.contains("notes.txt"));
}

#[test]
fn ignore_append_preset_and_layered_listing() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    commands::init::run(&proj).unwrap();

    let out = commands::ignore_cmd::run(&proj, Some("*.log"), None, false).unwrap();
    assert_eq!(out.json["action"], "added-line");
    let text = std::fs::read_to_string(proj.join("ferry.ignore")).unwrap();
    assert!(text.lines().any(|l| l == "*.log"));

    let err = commands::ignore_cmd::run(&proj, Some("[z-a]"), None, false).unwrap_err();
    assert_eq!(err.code, "bad-pattern");

    let out = commands::ignore_cmd::run(&proj, None, Some("claude"), false).unwrap();
    assert_eq!(out.json["action"], "applied-preset");
    let out2 = commands::ignore_cmd::run(&proj, None, Some("claude"), false).unwrap();
    assert_eq!(out2.json["action"], "applied-preset", "idempotent apply");
    let settings = read_settings(&proj);
    assert_eq!(settings.presets, vec!["claude".to_string()]);

    let err = commands::ignore_cmd::run(&proj, None, Some("nope"), false).unwrap_err();
    assert_eq!(err.code, "unknown-preset");

    let out = commands::ignore_cmd::run(&proj, None, None, true).unwrap();
    let layers = out.json["layers"].as_array().unwrap();
    let names: Vec<&str> = layers.iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "defaults (built-in)",
            "file ferry.ignore",
            "presets (applied)",
        ]
    );
    let preset_lines = layers[2]["lines"].as_array().unwrap();
    assert!(preset_lines.iter().any(|l| l == "!CLAUDE.md"));
    assert!(preset_lines.iter().any(|l| l == "**/*.log"));

    let rules = folder::load_rules(&proj, &settings).unwrap();

    assert!(rules.decided(&["app.log".to_string()], false));
}

#[test]
fn ignore_targets_explicit_folder_and_external_directory() {
    let (_e, _home, work) = setup();
    let proj = work.join("external_proj");
    commands::init::run(&proj).unwrap();

    let out = commands::ignore_cmd::run(&proj, Some("temp/"), None, false).unwrap();
    assert_eq!(out.json["action"], "added-line");
    assert_eq!(out.json["folder"], proj.display().to_string());
    assert!(proj.join("ferry.ignore").is_file());
    let text = std::fs::read_to_string(proj.join("ferry.ignore")).unwrap();
    assert!(text.contains("temp/"));

    let out_preset = commands::ignore_cmd::run(&proj, None, Some("claude"), false).unwrap();
    assert_eq!(out_preset.json["action"], "applied-preset");
    assert_eq!(out_preset.json["folder"], proj.display().to_string());

    let out_list = commands::ignore_cmd::run(&proj, None, None, true).unwrap();
    assert_eq!(out_list.json["action"], "list");
    assert_eq!(out_list.json["folder"], proj.display().to_string());
}

fn read_settings(proj: &Path) -> Settings {
    serde_json::from_str::<Settings>(
        &std::fs::read_to_string(proj.join(".ferry/settings.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn settings_format_version_is_stable_in_json() {
    let (_e, _home, work) = setup();
    let proj = work.join("proj");
    commands::init::run(&proj).unwrap();
    let raw = std::fs::read_to_string(proj.join(".ferry/settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["format_version"], SETTINGS_FORMAT_VERSION);
    assert!(v["presets"].is_array());
    assert!(v["overrides"].is_array());
    assert_eq!(v["honor_gitignore"], false);
}
