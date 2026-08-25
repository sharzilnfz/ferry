//! T-05 regression: hostile symlink targets are REFUSED loudly through the
//! ENGINE's apply path.
//!
//! The v1 pull stages both materialize through
//! [`ferry_sync::SessionApplier`], an adapter over
//! `ferry-materialize::Applier`. The deleted inline applier had NO policy:
//! `/etc`, `../../outside`, and `C:x` went straight to `symlink()`. These
//! tests prove the engine path now refuses every hostile class with a loud
//! `SymlinkRefused` error and zero filesystem effect — and that a benign
//! relative link still syncs.

use std::path::Path;

use ferry_materialize::MaterializeError;
use ferry_platform::LinkRefusal;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::diff::{Added, ChangeSet, EntryKind, EntryState};
use ferry_store::manifest::{serialize_tree_node, RootManifest, TreeNode};
use ferry_store::store::Store;
use ferry_store::BlobKind;
use ferry_sync::{ApplyError, SessionApplier};

fn open_store(dir: &Path) -> Store {
    std::fs::create_dir_all(dir).unwrap();
    Store::create(dir, [0u8; 32], Box::new(PassthroughCipher)).unwrap()
}

/// A root manifest whose tree is the canonical empty node.
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

/// Run one hostile entry through the same adapter call the v1 pull stages
/// make; require the typed refusal and an untouched working tree.
fn assert_refused(path: &[&str], target: &str, want_reason: LinkRefusal) {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let manifest = empty_manifest(&store);

    let err = SessionApplier::new(&store, &tree)
        .apply(&manifest, &symlink_added(path, target))
        .unwrap_err();

    match err {
        ApplyError::Materialize(MaterializeError::SymlinkRefused {
            path: p,
            target: t,
            reason,
        }) => {
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

    // Nothing may exist at the link's slot (or anywhere else).
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
    // From depth 1 ("sub/lnk"), three `..` components climb two levels
    // past the synced root.
    assert_refused(
        &["sub", "lnk"],
        "../../../outside",
        LinkRefusal::EscapesRoot,
    );
}

#[test]
fn windows_drive_prefixed_target_is_refused_through_the_engine_path() {
    // Drive-relative ("C:x") names a location outside the folder by
    // construction — refused on EVERY host, not just windows.
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
    // Positive control: the policy refuses HOSTILE targets only.
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir.path().join("store"));
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let manifest = empty_manifest(&store);
    // A real diff flattens added subtrees per-path, so `sub` would ride
    // along in the change set; mirror that by having it exist already.
    std::fs::create_dir_all(tree.join("sub")).unwrap();

    SessionApplier::new(&store, &tree)
        .apply(
            &manifest,
            &symlink_added(&["sub", "lnk"], "../shared/file.txt"),
        )
        .expect("in-tree relative links are legitimate");

    assert_eq!(
        std::fs::read_link(tree.join("sub/lnk"))
            .unwrap()
            .to_string_lossy(),
        "../shared/file.txt"
    );
}
