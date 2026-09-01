mod common;

use std::fs;
use std::time::Duration;

use common::EngineFixture;
use ferry_store::format::{hex, BlobKind};
use ferry_store::manifest::{parse_manifest, parse_tree_node, EntryPayload, RootManifest};

const SEED: u64 = 20260830;

fn extract_all_paths(store: &ferry_store::store::Store, man: &RootManifest) -> Vec<String> {
    let mut paths = Vec::new();
    let mut queue = vec![(man.root_tree_id, String::new())];
    while let Some((tree_id, prefix)) = queue.pop() {
        if tree_id == [0u8; 32] {
            continue;
        }
        if let Ok(bytes) = store.get(BlobKind::TreeNode, &tree_id) {
            if let Ok(node) = parse_tree_node(&bytes) {
                for entry in node.entries {
                    let entry_path = if prefix.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}/{}", prefix, entry.name)
                    };
                    match entry.payload {
                        EntryPayload::Dir { child_tree_id } => {
                            paths.push(format!("{entry_path}/"));
                            queue.push((child_tree_id, entry_path));
                        }
                        EntryPayload::File { .. } | EntryPayload::Symlink { .. } => {
                            paths.push(entry_path);
                        }
                    }
                }
            }
        }
    }
    paths
}

#[test]
fn default_ignore_patterns_are_excluded_from_manifests_and_wire() {
    let timeout = common::timeout_from_env();
    let fx = EngineFixture::start("ignore-defaults", SEED);

    let tree_a = fx.tree_a();
    fs::write(tree_a.join("allowed.txt"), b"allowed content\n").unwrap();
    fs::create_dir_all(tree_a.join("src")).unwrap();
    fs::write(tree_a.join("src/main.rs"), b"fn main() {}\n").unwrap();

    fs::write(tree_a.join(".env"), b"SECRET_KEY=12345\n").unwrap();
    fs::write(
        tree_a.join(".env.production"),
        b"DATABASE_URL=postgres://\n",
    )
    .unwrap();
    fs::create_dir_all(tree_a.join("node_modules/pkg")).unwrap();
    fs::write(
        tree_a.join("node_modules/pkg/index.js"),
        b"module.exports = {};\n",
    )
    .unwrap();
    fs::write(tree_a.join(".DS_Store"), b"\0\0\0\x01").unwrap();
    fs::write(tree_a.join("file.swp"), b"swap content\n").unwrap();
    fs::write(tree_a.join("backup~"), b"backup content\n").unwrap();

    fx.a.trigger_scan();
    let tree_b = fx.tree_b();
    let deadline = std::time::Instant::now() + timeout;
    let mut sleep_ms = 10u64;
    let mut ticks = 0u32;
    let agreed = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {:?}; state A={:?} B={:?}",
            timeout,
            fx.a.stats(),
            fx.b.stats()
        );
        let got_allowed =
            tree_b.join("allowed.txt").is_file() && tree_b.join("src/main.rs").is_file();
        if let (Some(a), Some(b)) = (fx.a.agreed_id(), fx.b.agreed_id()) {
            if a == b && a != [0u8; 32] && got_allowed && fx.converged() {
                break a;
            }
        }
        if ticks % 5 == 0 {
            fx.a.trigger_scan();
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
        ticks += 1;
    };

    assert_eq!(
        fs::read(tree_b.join("allowed.txt")).ok().as_deref(),
        Some(b"allowed content\n".as_slice())
    );
    assert_eq!(
        fs::read(tree_b.join("src/main.rs")).ok().as_deref(),
        Some(b"fn main() {}\n".as_slice())
    );

    assert!(!tree_b.join(".env").exists(), ".env must not sync");
    assert!(
        !tree_b.join(".env.production").exists(),
        ".env.production must not sync"
    );
    assert!(
        !tree_b.join("node_modules").exists(),
        "node_modules/ must not sync"
    );
    assert!(
        !tree_b.join(".DS_Store").exists(),
        ".DS_Store must not sync"
    );
    assert!(!tree_b.join("file.swp").exists(), "*.swp must not sync");
    assert!(!tree_b.join("backup~").exists(), "*~ must not sync");

    let store_dir = fx._dir.path().join("b/store");
    let id_b = ferry_sync::engine::device_identity_for_tag("ignore-defaults-b");
    if let Ok(store) = ferry_folder::open_or_create_test_store(&store_dir, &id_b) {
        if let Ok(bytes) = store.get(BlobKind::Manifest, &agreed) {
            let man = parse_manifest(&bytes).expect("parse agreed manifest");
            let all_paths = extract_all_paths(&store, &man);
            for p in &all_paths {
                assert!(
                    !p.starts_with(".env"),
                    "manifest contained ignored path: {p}"
                );
                assert!(
                    !p.starts_with("node_modules"),
                    "manifest contained ignored path: {p}"
                );
                assert!(
                    !std::path::Path::new(p)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("swp")),
                    "manifest contained ignored path: {p}"
                );
                assert!(!p.ends_with('~'), "manifest contained ignored path: {p}");
                assert_ne!(p, ".DS_Store", "manifest contained .DS_Store");
            }
        }
    }
}

#[test]
fn custom_ferry_ignore_rules_are_respected() {
    let timeout = common::timeout_from_env();
    let fx = EngineFixture::start_with_cfg_a("ignore-custom", SEED + 1, |cfg_a| {
        fs::write(
            cfg_a.tree_dir.join("ferry.ignore"),
            "target/\n*.log\nsecrets/\n",
        )
        .unwrap();
    });

    let tree_a = fx.tree_a();
    fs::write(tree_a.join("README.md"), b"# Project\n").unwrap();
    fs::create_dir_all(tree_a.join("target/debug")).unwrap();
    fs::write(tree_a.join("target/debug/app"), b"binary\n").unwrap();
    fs::write(tree_a.join("server.log"), b"log output\n").unwrap();
    fs::create_dir_all(tree_a.join("secrets")).unwrap();
    fs::write(tree_a.join("secrets/key.pem"), b"private key\n").unwrap();

    let tree_b = fx.tree_b();
    fx.a.trigger_scan();
    let deadline = std::time::Instant::now() + timeout;
    let mut sleep_ms = 10u64;
    let mut ticks = 0u32;
    let _agreed = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {:?}; state A={:?} B={:?}",
            timeout,
            fx.a.stats(),
            fx.b.stats()
        );
        let got_readme = tree_b.join("README.md").is_file();
        if let (Some(a), Some(b)) = (fx.a.agreed_id(), fx.b.agreed_id()) {
            if a == b && a != [0u8; 32] && got_readme && fx.converged() {
                break a;
            }
        }
        if ticks % 5 == 0 {
            fx.a.trigger_scan();
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
        ticks += 1;
    };

    assert!(tree_b.join("README.md").is_file());
    assert!(tree_b.join("ferry.ignore").is_file());
    assert!(!tree_b.join("target").exists(), "target/ must not sync");
    assert!(!tree_b.join("server.log").exists(), "*.log must not sync");
    assert!(!tree_b.join("secrets").exists(), "secrets/ must not sync");
}

#[test]
fn settings_presets_and_overrides_are_respected() {
    let timeout = common::timeout_from_env();
    let fx = EngineFixture::start_with_cfg_a("ignore-presets", SEED + 3, |cfg_a| {
        fs::create_dir_all(cfg_a.tree_dir.join(".ferry")).unwrap();
        let settings = ferry_folder::folder::Settings {
            format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
            folder_id: hex(&cfg_a.folder_id),
            honor_gitignore: false,
            presets: vec!["claude".to_string()],
            overrides: vec!["custom_build/".to_string(), "*.cache".to_string()],
        };
        ferry_folder::folder::save_settings(&cfg_a.tree_dir, &settings).unwrap();
        ferry_folder::folder::save_settings(&cfg_a.store_dir, &settings).unwrap();
    });

    let tree_a = fx.tree_a();
    fs::write(tree_a.join("CLAUDE.md"), b"# Claude instructions\n").unwrap();
    fs::create_dir_all(tree_a.join("skills")).unwrap();
    fs::write(tree_a.join("skills/skill.md"), b"skill\n").unwrap();

    fs::create_dir_all(tree_a.join("telemetry")).unwrap();
    fs::write(tree_a.join("telemetry/events.json"), b"{}\n").unwrap();
    fs::write(tree_a.join("output.log"), b"log\n").unwrap();

    fs::create_dir_all(tree_a.join("custom_build")).unwrap();
    fs::write(tree_a.join("custom_build/out.bin"), b"\0\0").unwrap();
    fs::write(tree_a.join("temp.cache"), b"cached data\n").unwrap();

    let tree_b = fx.tree_b();
    fx.a.trigger_scan();
    let deadline = std::time::Instant::now() + timeout;
    let mut sleep_ms = 10u64;
    let mut ticks = 0u32;
    let _agreed = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {:?}; state A={:?} B={:?}",
            timeout,
            fx.a.stats(),
            fx.b.stats()
        );
        let got_claude =
            tree_b.join("CLAUDE.md").is_file() && tree_b.join("skills/skill.md").is_file();
        if let (Some(a), Some(b)) = (fx.a.agreed_id(), fx.b.agreed_id()) {
            if a == b && a != [0u8; 32] && got_claude && fx.converged() {
                break a;
            }
        }
        if ticks % 5 == 0 {
            fx.a.trigger_scan();
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
        ticks += 1;
    };

    assert!(tree_b.join("CLAUDE.md").is_file());
    assert!(tree_b.join("skills/skill.md").is_file());
    assert!(
        !tree_b.join("telemetry").exists(),
        "telemetry/ must not sync"
    );
    assert!(
        !tree_b.join("output.log").exists(),
        "*.log must not sync under claude preset"
    );
    assert!(
        !tree_b.join("custom_build").exists(),
        "custom_build/ must not sync"
    );
    assert!(!tree_b.join("temp.cache").exists(), "*.cache must not sync");
}

#[test]
fn quarantine_files_are_never_ignored() {
    let timeout = common::timeout_from_env();
    let fx = EngineFixture::start_with_cfg_a("ignore-quarantine", SEED + 2, |cfg_a| {
        fs::write(cfg_a.tree_dir.join("ferry.ignore"), "*conflict*\n*.txt\n").unwrap();
    });

    let tree_a = fx.tree_a();
    let quarantine_name = "doc.txt.ferry-conflict.dev1-20260830";
    fs::write(tree_a.join(quarantine_name), b"quarantined content\n").unwrap();

    let tree_b = fx.tree_b();
    fx.a.trigger_scan();
    let deadline = std::time::Instant::now() + timeout;
    let mut sleep_ms = 10u64;
    let mut ticks = 0u32;
    let _agreed = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {:?}; state A={:?} B={:?}",
            timeout,
            fx.a.stats(),
            fx.b.stats()
        );
        let got_quarantine = tree_b.join(quarantine_name).is_file();
        if let (Some(a), Some(b)) = (fx.a.agreed_id(), fx.b.agreed_id()) {
            if a == b && a != [0u8; 32] && got_quarantine && fx.converged() {
                break a;
            }
        }
        if ticks % 5 == 0 {
            fx.a.trigger_scan();
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
        ticks += 1;
    };

    assert!(
        tree_b.join(quarantine_name).is_file(),
        "quarantine file must sync regardless of ignore rules (ADR-0004)"
    );
}
