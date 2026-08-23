//! Cached per-directory state that lets incremental passes splice rebuilt
//! subtrees into the tree without touching untouched directories.
//!
//! Seeded once from the store (after the initial full scan) by parsing the
//! stored tree nodes back out — manifests are already on disk, so the cache
//! costs no extra IO beyond metadata reads. Every entry pairs a tree node
//! with its BLAKE3 address so a rebuilt parent can point at either the
//! cached child (untouched) or the freshly rebuilt one.

use std::collections::HashMap;

use ferry_store::manifest::{parse_tree_node, TreeEntry, TreeNode};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

use crate::error::ScanError;
use crate::policy::RelPath;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedDir {
    pub id: BlobId,
    pub node: TreeNode,
}

#[derive(Default)]
pub(crate) struct DirCache {
    dirs: HashMap<RelPath, CachedDir>,
}

impl DirCache {
    pub(crate) fn new() -> Self {
        DirCache::default()
    }

    /// Rebuild the whole cache from a stored tree. Used after full snapshots
    /// (initial scan, audit, overflow recovery): drift between cache and
    /// disk cannot survive a reseed.
    pub(crate) fn reseed(
        &mut self,
        store: &Store,
        root_tree_id: &BlobId,
    ) -> Result<(), ScanError> {
        self.dirs.clear();
        let root = parse_tree_node(&store.get(BlobKind::TreeNode, root_tree_id)?)?;
        self.insert_recursive(store, Vec::new(), *root_tree_id, root)
    }

    fn insert_recursive(
        &mut self,
        store: &Store,
        rel: RelPath,
        id: BlobId,
        node: TreeNode,
    ) -> Result<(), ScanError> {
        for e in &node.entries {
            if let ferry_store::manifest::EntryPayload::Dir { child_tree_id } = &e.payload {
                let child_rel = {
                    let mut r = rel.clone();
                    r.push(e.name.clone());
                    r
                };
                if !self.dirs.contains_key(&child_rel) {
                    let child =
                        parse_tree_node(&store.get(BlobKind::TreeNode, child_tree_id)?)?;
                    self.insert_recursive(store, child_rel, *child_tree_id, child)?;
                }
            }
        }
        self.dirs.insert(rel, CachedDir { id, node });
        Ok(())
    }

    pub(crate) fn node(&self, rel: &RelPath) -> Option<&CachedDir> {
        self.dirs.get(rel)
    }

    pub(crate) fn insert(&mut self, rel: RelPath, dir: CachedDir) {
        self.dirs.insert(rel, dir);
    }

    /// The previous entry recorded for `name` inside cached dir `parent`.
    /// This is what the size/mtime/exec short-circuit compares against.
    pub(crate) fn child_entry(&self, parent: &RelPath, name: &str) -> Option<&TreeEntry> {
        self.dirs.get(parent)?.node.entries.iter().find(|e| e.name == name)
    }

    /// Drop `prefix` and everything below it (deleted or renamed-away
    /// subtrees). Cache coherence: stale records must never satisfy a later
    /// short-circuit check.
    pub(crate) fn remove_prefix(&mut self, prefix: &RelPath) {
        self.dirs.retain(|k, _| !starts_with(k, prefix));
    }

    /// Direct children of `parent` currently cached.
    pub(crate) fn keys_under<'c>(
        &'c self,
        parent: &'c RelPath,
    ) -> impl Iterator<Item = &'c RelPath> {
        self.dirs.keys().filter(move |k| {
            k.len() == parent.len() + 1 && k[..parent.len()] == parent[..]
        })
    }

    /// Iterate all cached directories whose path is inside `subtree`.
    pub(crate) fn iter_within<'c>(
        &'c self,
        subtree: &'c RelPath,
    ) -> impl Iterator<Item = (&'c RelPath, &'c CachedDir)> {
        self.dirs
            .iter()
            .filter(move |(k, _)| starts_with(k, subtree))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.dirs.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// Component-vector prefix test; `[..]` is a prefix of everything.
pub(crate) fn starts_with(path: &RelPath, prefix: &RelPath) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == prefix[..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn key(parts: &[&str]) -> RelPath {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reseed_recovers_full_hierarchy_from_store() {
        let (_d, store) = fresh_store();
        let root = _d.path().join("t");
        write_file(&root.join("a.txt"), b"alpha", false, (1, 0));
        write_file(&root.join("sub/deep/b.txt"), b"beta", true, (2, 0));
        let out = ferry_store::snapshot::snapshot_dir(
            &store,
            poly_of(3),
            &root,
            &identity((1, 0)),
        )
        .unwrap();

        let mut cache = DirCache::new();
        cache.reseed(&store, &out.root_tree_id).unwrap();
        // Root, sub, sub/deep.
        assert_eq!(cache.len(), 3);

        let sub = cache.node(&key(&["sub"])).expect("sub cached");
        let deep = sub
            .node
            .entries
            .iter()
            .find(|e| e.name == "deep")
            .and_then(|e| match &e.payload {
                ferry_store::manifest::EntryPayload::Dir { child_tree_id } => Some(*child_tree_id),
                _ => None,
            })
            .expect("deep is a dir");
        assert_eq!(
            cache.node(&key(&["sub", "deep"])).unwrap().id,
            deep,
            "cached id matches parent's pointer"
        );
    }

    #[test]
    fn remove_prefix_drops_subtree_but_not_siblings() {
        let mut cache = DirCache::new();
        let mk = || CachedDir {
            id: [0u8; 32],
            node: TreeNode { entries: vec![] },
        };
        cache.insert(key(&["keep"]), mk());
        cache.insert(key(&["dead"]), mk());
        cache.insert(key(&["dead", "inner"]), mk());
        cache.insert(key(&["deadline"]), mk());

        cache.remove_prefix(&key(&["dead"]));
        assert!(cache.node(&key(&["keep"])).is_some());
        assert!(cache.node(&key(&["dead"])).is_none());
        assert!(cache.node(&key(&["dead", "inner"])).is_none());
        assert!(
            cache.node(&key(&["deadline"])).is_some(),
            "component-prefix lookalikes survive"
        );
    }

    #[test]
    fn child_entry_finds_short_circuit_records() {
        let (_d, store) = fresh_store();
        let root = _d.path().join("t");
        write_file(&root.join("x.txt"), b"data", false, (9, 9));
        let out = ferry_store::snapshot::snapshot_dir(
            &store,
            poly_of(4),
            &root,
            &identity((1, 0)),
        )
        .unwrap();
        let mut cache = DirCache::new();
        cache.reseed(&store, &out.root_tree_id).unwrap();
        let e = cache.child_entry(&key(&[]), "x.txt").expect("record present");
        assert_eq!(e.mtime_sec, 9);
        assert!(cache.child_entry(&key(&[]), "missing.txt").is_none());
    }
}
