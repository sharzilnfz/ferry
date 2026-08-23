//! Mtime-noise normalization: the definition of "same tree" used by the
//! correctness invariant and its tests.
//!
//! Two manifests are **equivalent modulo mtime noise** when stripping every
//! entry's mtime (files, dirs, symlinks alike) makes their trees serialize
//! identically. Content identity (chunk-id sequences), names, types, exec
//! bits, and symlink targets all still count; only timestamps are forgiven.
//!
//! Why: filesystems and test harnesses bump directory mtimes as a side
//! effect of unrelated operations, and an incremental pass legitimately
//! re-derives mtimes from disk. A comparison that forgave nothing would flag
//! phantom drift; one that forgave content would hide real bugs.

use std::collections::HashMap;

use blake3::Hasher;
use ferry_store::manifest::{
    parse_tree_node, serialize_tree_node, EntryPayload, TreeEntry, TreeNode,
};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

use crate::error::ScanError;

/// BLAKE3 of the subtree serialization with all mtimes zeroed, computed
/// recursively (dir children replaced by their own canonical ids).
pub fn canonical_tree_id(store: &Store, tree_id: &BlobId) -> Result<BlobId, ScanError> {
    let mut memo = HashMap::new();
    canonical_inner(store, tree_id, &mut memo)
}

fn canonical_inner(
    store: &Store,
    tree_id: &BlobId,
    memo: &mut HashMap<BlobId, BlobId>,
) -> Result<BlobId, ScanError> {
    if let Some(hit) = memo.get(tree_id) {
        return Ok(*hit);
    }
    let node = parse_tree_node(&store.get(BlobKind::TreeNode, tree_id)?)?;
    let mut stripped = TreeNode {
        entries: node.entries.iter().map(strip_entry).collect(),
    };
    for e in &mut stripped.entries {
        if let EntryPayload::Dir { child_tree_id } = &mut e.payload {
            *child_tree_id = canonical_inner(store, child_tree_id, memo)?;
        }
    }
    let mut hasher = Hasher::new();
    hasher.update(&serialize_tree_node(&stripped));
    let id = *hasher.finalize().as_bytes();
    memo.insert(*tree_id, id);
    Ok(id)
}

fn strip_entry(e: &TreeEntry) -> TreeEntry {
    TreeEntry {
        name: e.name.clone(),
        exec: e.exec,
        mtime_sec: 0,
        mtime_nsec: 0,
        payload: match &e.payload {
            EntryPayload::File { size, chunks } => EntryPayload::File {
                size: *size,
                chunks: chunks.clone(),
            },
            EntryPayload::Dir { child_tree_id } => EntryPayload::Dir {
                child_tree_id: *child_tree_id,
            },
            EntryPayload::Symlink { target } => EntryPayload::Symlink {
                target: target.clone(),
            },
        },
    }
}

/// The invariant comparator: true when the two roots describe the same tree
/// ignoring mtimes.
pub fn equivalent_modulo_mtime(store: &Store, a: &BlobId, b: &BlobId) -> Result<bool, ScanError> {
    Ok(canonical_tree_id(store, a)? == canonical_tree_id(store, b)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ferry_store::snapshot::snapshot_dir;

    fn snap(store: &Store, root: &std::path::Path, stamp: i64) -> BlobId {
        snapshot_dir(store, poly_of(5), root, &identity((stamp, 0)))
            .unwrap()
            .root_tree_id
    }

    #[test]
    fn identical_trees_are_equivalent() {
        let (_d, store) = fresh_store();
        let root = _d.path().join("t");
        write_file(&root.join("x.txt"), b"same", false, (1, 2));
        write_file(&root.join("d/y.txt"), b"nested", true, (3, 4));
        let t = snap(&store, &root, 1);
        assert!(equivalent_modulo_mtime(&store, &t, &t).unwrap());
    }

    #[test]
    fn mtime_only_changes_are_forgiven() {
        let (_d, store) = fresh_store();
        let root_a = _d.path().join("a");
        let root_b = _d.path().join("b");
        write_file(&root_a.join("x.txt"), b"same", false, (111, 222));
        write_file(&root_b.join("x.txt"), b"same", false, (999, 888));
        let ta = snap(&store, &root_a, 1);
        let tb = snap(&store, &root_b, 2);
        assert!(
            equivalent_modulo_mtime(&store, &ta, &tb).unwrap(),
            "mtime-only difference must normalize away"
        );
        assert_ne!(ta, tb, "raw ids differ because raw serialized bytes differ");
    }

    #[test]
    fn content_and_exec_differences_survive_normalization() {
        let (_d, store) = fresh_store();
        let a = _d.path().join("a");
        let b = _d.path().join("b");
        let c = _d.path().join("c");
        write_file(&a.join("f"), b"one", false, (5, 0));
        write_file(&b.join("f"), b"two", false, (5, 0));
        write_file(&c.join("f"), b"one", true, (5, 0));
        let ta = snap(&store, &a, 1);
        let tb = snap(&store, &b, 2);
        let tc = snap(&store, &c, 3);

        assert!(!equivalent_modulo_mtime(&store, &ta, &tb).unwrap());
        assert!(
            !equivalent_modulo_mtime(&store, &ta, &tc).unwrap(),
            "exec bit is content-adjacent metadata, not noise"
        );
    }
}
