use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;
use tempfile::TempDir;

use ferry_ignore::secrets::WarningClass;
use ferry_ignore::{FerryIgnore, IgnoreConfig};
use ferry_scan::{EntryKind, IgnorePolicy, ScanConfig, ScanEngine, StoreHandle, WatchSignal};
use ferry_store::crypto::{PassthroughCipher, KEY_LEN};
use ferry_store::manifest::{parse_tree_node, EntryPayload};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

fn key() -> [u8; KEY_LEN] {
    core::array::from_fn(|i| i as u8)
}

fn poly() -> ferry_store::chunker::ValidatedPoly {
    ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(4242))
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn collect_paths(store: &Store, id: &BlobId, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    let bytes = store.get(BlobKind::TreeNode, id).unwrap();
    let node = parse_tree_node(&bytes).unwrap();
    for e in node.entries {
        match &e.payload {
            EntryPayload::Dir { child_tree_id } => {
                prefix.push(e.name.clone());
                out.push(prefix.clone());
                collect_paths(store, child_tree_id, prefix, out);
                prefix.pop();
            }
            _ => {
                prefix.push(e.name.clone());
                out.push(prefix.clone());
                prefix.pop();
            }
        }
    }
}

fn rel(path: &str) -> Vec<String> {
    path.split('/').map(str::to_string).collect()
}

#[test]
fn mixed_tree_manifests_exactly_the_allowed_paths() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    write_file(
        &root.join("ferry.ignore"),
        b"*.log\n!node_modules/\n!.env\nscratch/\n",
    );
    write_file(&root.join("README.md"), b"hello");
    write_file(
        &root.join(".env"),
        b"AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n",
    );
    write_file(&root.join("app/main.rs"), b"fn main() {}\n");
    write_file(&root.join("debug.log"), b"noise");
    write_file(&root.join("scratch/junk.bin"), b"ignored churn target");
    write_file(&root.join("sub/ferry.ignore"), b"!keep.log\n");
    write_file(&root.join("sub/keep.log"), b"kept by deeper file");
    write_file(&root.join("sub/other.log"), b"still ignored at root layer");
    write_file(
        &root.join("node_modules/pkg/index.js"),
        b"module.exports={}",
    );
    write_file(&root.join(".DS_Store"), b"finder junk");
    write_file(&root.join("notes.txt~"), b"editor junk");

    let store_dir = TempDir::new().unwrap();
    let store =
        Arc::new(Store::create(store_dir.path(), key(), Box::new(PassthroughCipher)).unwrap());
    let handle = StoreHandle {
        store: store.clone(),
        poly: poly(),
        folder_id: [7; 16],
        device_id: [9; 32],
    };

    let cfg = IgnoreConfig::default();
    let policy = Arc::new(FerryIgnore::new(&root, &cfg).unwrap());

    let scan_cfg = ScanConfig {
        quiet_window: Duration::from_millis(10),
        audit_interval: Duration::from_hours(1),
        poll_interval: Duration::from_millis(50),
        parent_manifest_id: None,
    };
    let engine = ScanEngine::watch_with(root.clone(), handle, scan_cfg, policy.clone()).unwrap();

    let current = engine.current().expect("initial scan published");
    let mut paths = Vec::new();
    collect_paths(&store, &current.root_tree_id, &mut Vec::new(), &mut paths);
    paths.sort();

    let mut want: Vec<Vec<String>> = [
        ".env",
        "README.md",
        "app",
        "app/main.rs",
        "ferry.ignore",
        "node_modules",
        "node_modules/pkg",
        "node_modules/pkg/index.js",
        "sub",
        "sub/ferry.ignore",
        "sub/keep.log",
    ]
    .iter()
    .map(|p| rel(p))
    .collect();
    want.sort();
    assert_eq!(paths, want, "manifest must contain exactly the allowed set");

    engine.stop();

    write_file(&root.join("scratch/junk.bin"), b"churn");
    engine.debug_inject_signal(WatchSignal::Changed(vec![rel("scratch/junk.bin")]));
    let run = engine.scan_once().unwrap();
    assert!(
        run.published.is_none(),
        "ignored-subtree event must not produce a manifest"
    );
    assert_eq!(run.stats.bytes_chunked, 0);

    write_file(&root.join("README.md"), b"hello v2");
    engine.debug_inject_signal(WatchSignal::Changed(vec![rel("README.md")]));
    let run = engine.scan_once().unwrap();
    let published = run.published.expect("allowed-file change must publish");
    assert!(run.stats.files_rehashed >= 1);
    assert_eq!(
        published.manifest.parent_manifest_id, current.manifest_id,
        "manifest lineage chains"
    );

    let warnings = ferry_ignore::secrets::scan_for_secrets(&policy, &root);
    assert!(warnings
        .iter()
        .any(|w| w.class == WarningClass::EnvFile && w.path == vec![".env"]));
    assert!(warnings.iter().any(|w| w.class == WarningClass::AwsKey));
    assert!(warnings
        .iter()
        .all(|w| !w.preview.contains("AKIAIOSFODNN7EXAMPLE")));
}

#[test]
fn policy_filters_poll_sweeps_like_walker_events() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_file(&root.join("ferry.ignore"), b"cache/\n");
    write_file(&root.join("keep.txt"), b"v1");
    write_file(&root.join("cache/blob.bin"), b"cached junk v1");

    let policy = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();

    assert!(policy.ignored(&rel("cache"), EntryKind::Dir));
    assert!(policy.ignored(&rel("cache/blob.bin"), EntryKind::File));
    assert!(!policy.ignored(&rel("keep.txt"), EntryKind::File));
}
