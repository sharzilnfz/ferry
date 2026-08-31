









use std::path::Path;
use std::sync::Arc;

use ferry_materialize::{Applier, MaterializeError};
use ferry_platform::LinkRefusal;
use ferry_store::diff::{Added, ChangeSet, EntryKind, EntryState};
use ferry_store::manifest::{serialize_tree_node, RootManifest, TreeNode};
use ferry_store::store::Store;
use ferry_store::BlobKind;

fn open_store(dir: &Path) -> Arc<Store> {
    
    let identity = ferry_crypto::identity::DeviceIdentity::from_secret_bytes(&[0xB2u8; 32]);
    ferry_folder::open_or_create_test_store(dir, &identity).unwrap()
}


fn empty_manifest(store: &Store) -> RootManifest {
    let bytes = serialize_tree_node(&TreeNode {
        entries: Vec::new(),
    });
    let id = store.put_meta(BlobKind::TreeNode, &bytes).unwrap();
    RootManifest {
        folder_id: [0; 16],
        device_id: [0; 32],
        created_sec: 0,
        created_nsec: 0,
        root_tree_id: id,
        parent_manifest_id: [0; 32],
    }
}

fn symlink_added(path: &[&str], target: &str) -> ChangeSet {
    ChangeSet {
        added: vec![Added {
            path: path.iter().copied().map(str::to_string).collect(),
            state: EntryState {
                kind: EntryKind::Symlink,
                exec: false,
                mtime_sec: 1_700_000_000,
                mtime_nsec: 42,
                chunks: Vec::new(),
                target: Some(target.to_string()),
            },
        }],
        ..Default::default()
    }
}



fn assert_refused(path: &[&str], target: &str, want_reason: LinkRefusal) {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let manifest = empty_manifest(&store);

    let err = Applier::new(&store, &tree)
        .apply_session_change_set(&symlink_added(path, target), &manifest.root_tree_id)
        .unwrap_err();

    match err {
        MaterializeError::SymlinkRefused {
            path: p,
            target: t,
            reason,
        } => {
            assert_eq!(
                p.split('/').map(str::to_string).collect::<Vec<_>>(),
                path.iter().copied().map(str::to_string).collect::<Vec<_>>(),
                "refusal names the offending path"
            );
            assert_eq!(t.as_str(), target);
            assert_eq!(reason, want_reason, "refusal must name the reason");
        }
        other => panic!("expected SymlinkRefused for {target:?}, got {other}"),
    }

    
    let mut probe = tree.clone();
    for c in path {
        probe.push(c);
    }
    assert!(probe.symlink_metadata().is_err(), "nothing materialized");
}

#[test]
fn absolute_target_is_refused_through_the_engine_path() {
    assert_refused(&["escape"], "/etc/passwd", LinkRefusal::AbsoluteTarget);
}

#[test]
fn escaping_dotdot_target_is_refused_through_the_engine_path() {
    
    
    assert_refused(
        &["sub", "lnk"],
        "../../../outside",
        LinkRefusal::EscapesRoot,
    );
}

#[test]
fn windows_drive_prefixed_target_is_refused_through_the_engine_path() {
    
    
    assert_refused(&["drive"], "C:x", LinkRefusal::AbsoluteTarget);
}

#[test]
fn unc_and_backslash_root_targets_are_refused_too() {
    assert_refused(&["unc"], "\\\\server\\share", LinkRefusal::AbsoluteTarget);
    assert_refused(&["rooted"], "\\etc", LinkRefusal::AbsoluteTarget);
}

#[test]
#[cfg(unix)]
fn benign_relative_link_still_applies_through_the_engine_path() {
    
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let manifest = empty_manifest(&store);
    
    
    std::fs::create_dir_all(tree.join("sub")).unwrap();

    Applier::new(&store, &tree)
        .apply_session_change_set(
            &symlink_added(&["sub", "lnk"], "../shared/file.txt"),
            &manifest.root_tree_id,
        )
        .expect("in-tree relative links are legitimate");

    assert_eq!(
        std::fs::read_link(tree.join("sub/lnk"))
            .unwrap()
            .to_string_lossy(),
        "../shared/file.txt"
    );
}
