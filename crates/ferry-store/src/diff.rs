//! Manifest diffing (T-003): change sets computed from metadata alone.
//!
//! `docs/store-format.md` and ADR-0001 make delta detection a manifest
//! comparison: two trees are compared by their chunk-id sequences and entry
//! metadata, and no data blob is ever read. This module is the contract for
//! that comparison.
//!
//! Classification rules (documented decisions for T-003):
//!
//! - Files: a different chunk-id sequence is `content_modified`; identical
//!   chunks with a changed exec bit or mtime are `metadata_modified`.
//! - Symlinks: the target IS the content, so retargeting is
//!   `content_modified`; an mtime-only bump is `metadata_modified`.
//! - Directories are compared by subtree identity (child tree id). A
//!   directory's own mtime changing while its listing stays identical is NOT
//!   reported: dir mtimes churn with any nested edit, and materializers set
//!   directory times last anyway.
//! - Type changes (file→dir etc.) surface once, in `type_changed`, carrying
//!   both the before and after states — not as remove+add.
//! - Added/removed subtrees are flattened per path, parents before children.
//! - Every bucket is sorted ascending by NFC component vector, so output is
//!   deterministic regardless of traversal order.
//!
//! Equal root ids short-circuit to an empty change set, and equal child ids
//! prune whole subtrees, so comparing near-identical manifests is cheap.

use std::cmp::Ordering;

use thiserror::Error;

use crate::format::{put_bytes, put_i64, put_u32, put_u64, put_u8, BlobId, BlobKind, Reader};
use crate::manifest::{parse_tree_node, EntryPayload, RootManifest, TreeEntry, TreeNode};
use crate::store::{Store, StoreError};

#[derive(Debug, Error)]
pub enum DiffError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("stored tree node failed validation: {0}")]
    Tree(#[from] crate::manifest::ManifestError),
    #[error("change-set payload corrupt: {0}")]
    Encoding(&'static str),
}

/// What kind of filesystem object an entry describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

impl EntryKind {
    fn from_payload(p: &EntryPayload) -> Self {
        match p {
            EntryPayload::File { .. } => EntryKind::File,
            EntryPayload::Dir { .. } => EntryKind::Dir,
            EntryPayload::Symlink { .. } => EntryKind::Symlink,
        }
    }

    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(EntryKind::File),
            1 => Some(EntryKind::Dir),
            2 => Some(EntryKind::Symlink),
            _ => None,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            EntryKind::File => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink => 2,
        }
    }
}

/// Everything a materializer needs to act on one changed path: what it is,
/// its mode/exec bit, its mtime, its ordered chunk list (files), and its
/// target (symlinks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryState {
    pub kind: EntryKind,
    pub exec: bool,
    pub mtime_sec: i64,
    pub mtime_nsec: u32,
    /// Ordered `(chunk_id, chunk_len)` pairs; files only, empty otherwise.
    pub chunks: Vec<(BlobId, u64)>,
    /// Symlink target; symlinks only.
    pub target: Option<String>,
}

impl EntryState {
    pub(crate) fn of(e: &TreeEntry) -> Self {
        let kind = EntryKind::from_payload(&e.payload);
        let (chunks, target) = match &e.payload {
            EntryPayload::File { chunks, .. } => (chunks.clone(), None),
            EntryPayload::Dir { .. } => (Vec::new(), None),
            EntryPayload::Symlink { target } => (Vec::new(), Some(target.clone())),
        };
        EntryState {
            kind,
            exec: e.exec,
            mtime_sec: e.mtime_sec,
            mtime_nsec: e.mtime_nsec,
            chunks,
            target,
        }
    }
}

/// A path as NFC `/`-separated components (never a joined string on the
/// wire; use [`join_path`] for display).
pub type CompPath = Vec<String>;

/// Join components for display/logging only.
pub fn join_path(parts: &[String]) -> String {
    parts.join("/")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    pub path: CompPath,
    pub state: EntryState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub path: CompPath,
    pub state: EntryState,
}

/// One path whose content or type differs between two states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modified {
    pub path: CompPath,
    pub before: EntryState,
    pub after: EntryState,
}

/// The complete difference between two manifests or tree nodes.
///
/// Bucket order within each vector is deterministic: ascending by component
/// vector, parents before children.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub added: Vec<Added>,
    pub removed: Vec<Removed>,
    pub content_modified: Vec<Modified>,
    pub metadata_modified: Vec<Modified>,
    pub type_changed: Vec<Modified>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.content_modified.is_empty()
            && self.metadata_modified.is_empty()
            && self.type_changed.is_empty()
    }

    fn sort_all(&mut self) {
        self.added.sort_by(|a, b| a.path.cmp(&b.path));
        self.removed.sort_by(|a, b| a.path.cmp(&b.path));
        self.content_modified.sort_by(|a, b| a.path.cmp(&b.path));
        self.metadata_modified.sort_by(|a, b| a.path.cmp(&b.path));
        self.type_changed.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// Diff two root manifests through the store. Identical roots return an
/// empty set immediately; neither file bytes nor the source tree are ever
/// touched — only tree nodes and manifests (metadata blobs) are read.
pub fn diff_manifests(
    store: &Store,
    older: &RootManifest,
    newer: &RootManifest,
) -> Result<ChangeSet, DiffError> {
    if older.root_tree_id == newer.root_tree_id {
        return Ok(ChangeSet::default());
    }
    diff_roots(store, &older.root_tree_id, &newer.root_tree_id)
}

/// Diff starting from two arbitrary tree-node addresses.
pub fn diff_roots(
    store: &Store,
    older_root: &BlobId,
    newer_root: &BlobId,
) -> Result<ChangeSet, DiffError> {
    let mut cs = ChangeSet::default();
    diff_tree_ids(
        store,
        Some(older_root),
        Some(newer_root),
        Vec::new(),
        &mut cs,
    )?;
    cs.sort_all();
    Ok(cs)
}

fn load_node(store: &Store, id: &BlobId) -> Result<TreeNode, DiffError> {
    let bytes = store.get(BlobKind::TreeNode, id)?;
    Ok(parse_tree_node(&bytes)?)
}

fn child_path(prefix: &[String], name: &str) -> CompPath {
    let mut p = prefix.to_vec();
    p.push(name.to_string());
    p
}

fn diff_tree_ids(
    store: &Store,
    older: Option<&BlobId>,
    newer: Option<&BlobId>,
    prefix: CompPath,
    out: &mut ChangeSet,
) -> Result<(), DiffError> {
    // Equal subtrees prune: the whole branch below an unchanged id is
    // skipped without loading anything.
    match (older, newer) {
        (Some(a), Some(b)) if a == b => return Ok(()),
        (None, None) => return Ok(()),
        _ => {}
    }
    let ta = match older {
        Some(id) => Some(load_node(store, id)?),
        None => None,
    };
    let tb = match newer {
        Some(id) => Some(load_node(store, id)?),
        None => None,
    };
    diff_nodes(store, ta.as_ref(), tb.as_ref(), &prefix, out)
}

fn diff_nodes(
    store: &Store,
    older: Option<&TreeNode>,
    newer: Option<&TreeNode>,
    prefix: &[String],
    out: &mut ChangeSet,
) -> Result<(), DiffError> {
    // Entries are sorted by name bytes, so a merge walk pairs them in one
    // pass with no allocations.
    let a: &[TreeEntry] = older.map(|n| n.entries.as_slice()).unwrap_or(&[]);
    let b: &[TreeEntry] = newer.map(|n| n.entries.as_slice()).unwrap_or(&[]);
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        let ord = match (i < a.len(), j < b.len()) {
            (true, true) => a[i].name.as_bytes().cmp(b[j].name.as_bytes()),
            (true, false) => Ordering::Less,
            (false, _) => Ordering::Greater,
        };
        match ord {
            Ordering::Less => {
                flatten_removed_entry(store, &a[i], prefix, out)?;
                i += 1;
            }
            Ordering::Greater => {
                flatten_added_entry(store, &b[j], prefix, out)?;
                j += 1;
            }
            Ordering::Equal => {
                compare_entry(store, &a[i], &b[j], prefix, out)?;
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

fn flatten_added_entry(
    store: &Store,
    e: &TreeEntry,
    prefix: &[String],
    out: &mut ChangeSet,
) -> Result<(), DiffError> {
    let path = child_path(prefix, &e.name);
    out.added.push(Added {
        path: path.clone(),
        state: EntryState::of(e),
    });
    if let EntryPayload::Dir { child_tree_id } = &e.payload {
        diff_tree_ids(store, None, Some(child_tree_id), path, out)?;
    }
    Ok(())
}

fn flatten_removed_entry(
    store: &Store,
    e: &TreeEntry,
    prefix: &[String],
    out: &mut ChangeSet,
) -> Result<(), DiffError> {
    let path = child_path(prefix, &e.name);
    out.removed.push(Removed {
        path: path.clone(),
        state: EntryState::of(e),
    });
    if let EntryPayload::Dir { child_tree_id } = &e.payload {
        diff_tree_ids(store, Some(child_tree_id), None, path, out)?;
    }
    Ok(())
}

fn compare_entry(
    store: &Store,
    ea: &TreeEntry,
    eb: &TreeEntry,
    prefix: &[String],
    out: &mut ChangeSet,
) -> Result<(), DiffError> {
    let path = child_path(prefix, &ea.name);
    match (&ea.payload, &eb.payload) {
        (EntryPayload::Dir { child_tree_id: a }, EntryPayload::Dir { child_tree_id: b }) => {
            if a != b {
                // The listing changed somewhere below; recurse. Equal ids
                // never get here.
                diff_tree_ids(store, Some(a), Some(b), path, out)?;
            }
            // A directory's own mtime changing while its subtree identity
            // stays equal is deliberately not reported (see module docs).
        }
        (EntryPayload::File { chunks: ca, .. }, EntryPayload::File { chunks: cb, .. }) => {
            if ca != cb {
                out.content_modified.push(Modified {
                    path,
                    before: EntryState::of(ea),
                    after: EntryState::of(eb),
                });
            } else if ea.exec != eb.exec
                || ea.mtime_sec != eb.mtime_sec
                || ea.mtime_nsec != eb.mtime_nsec
            {
                out.metadata_modified.push(Modified {
                    path,
                    before: EntryState::of(ea),
                    after: EntryState::of(eb),
                });
            }
        }
        (EntryPayload::Symlink { target: a }, EntryPayload::Symlink { target: b }) => {
            if a != b {
                // The target IS the content of a symlink.
                out.content_modified.push(Modified {
                    path,
                    before: EntryState::of(ea),
                    after: EntryState::of(eb),
                });
            } else if ea.mtime_sec != eb.mtime_sec || ea.mtime_nsec != eb.mtime_nsec {
                out.metadata_modified.push(Modified {
                    path,
                    before: EntryState::of(ea),
                    after: EntryState::of(eb),
                });
            }
        }
        _ => {
            // file<->dir, file<->symlink, dir<->symlink: one explicit entry
            // carrying both sides, not a remove+add pair.
            out.type_changed.push(Modified {
                path,
                before: EntryState::of(ea),
                after: EntryState::of(eb),
            });
        }
    }
    Ok(())
}

// --- internal change-set codec -------------------------------------------
//
// A small deterministic binary encoding of [`ChangeSet`] for tests, logs,
// and passing change sets between processes during development. This is an
// INTERNAL convenience: it is deliberately NOT part of the compatibility
// contract in docs/store-format.md and may change without a version bump.

const CHANGESET_MAGIC: [u8; 4] = *b"FCS1";

fn put_state(out: &mut Vec<u8>, s: &EntryState) {
    put_u8(out, s.kind.to_u8());
    put_u8(out, u8::from(s.exec));
    put_i64(out, s.mtime_sec);
    put_u32(out, s.mtime_nsec);
    put_u32(out, s.chunks.len() as u32);
    for (id, len) in &s.chunks {
        put_bytes(out, id);
        put_u64(out, *len);
    }
    match &s.target {
        Some(t) => {
            put_u8(out, 1);
            put_u32(out, t.len() as u32);
            put_bytes(out, t.as_bytes());
        }
        None => put_u8(out, 0),
    }
}

fn parse_state(r: &mut Reader<'_>) -> Result<EntryState, DiffError> {
    let bad = |_: &'static str| DiffError::Encoding("bad entry state");
    let kind = EntryKind::from_u8(r.u8().map_err(|_| bad("kind"))?).ok_or(bad("kind"))?;
    let exec = match r.u8().map_err(|_| bad("exec"))? {
        0 => false,
        1 => true,
        _ => return Err(bad("exec")),
    };
    let mtime_sec = r.i64().map_err(|_| bad("sec"))?;
    let mtime_nsec = r.u32().map_err(|_| bad("nsec"))?;
    let n_chunks = r.u32().map_err(|_| bad("chunk count"))? as usize;
    let mut chunks = Vec::with_capacity(n_chunks);
    for _ in 0..n_chunks {
        let id = r.array::<32>().map_err(|_| bad("chunk id"))?;
        let len = r.u64().map_err(|_| bad("chunk len"))?;
        chunks.push((id, len));
    }
    let target = match r.u8().map_err(|_| bad("target flag"))? {
        0 => None,
        1 => {
            let n = r.u32().map_err(|_| bad("target len"))? as usize;
            let bytes = r.take(n).map_err(|_| bad("target"))?;
            Some(String::from_utf8(bytes.to_vec()).map_err(|_| bad("target utf8"))?)
        }
        _ => return Err(bad("target flag")),
    };
    Ok(EntryState {
        kind,
        exec,
        mtime_sec,
        mtime_nsec,
        chunks,
        target,
    })
}

fn put_path(out: &mut Vec<u8>, path: &[String]) {
    put_u32(out, path.len() as u32);
    for c in path {
        put_u32(out, c.len() as u32);
        put_bytes(out, c.as_bytes());
    }
}

fn parse_path(r: &mut Reader<'_>) -> Result<CompPath, DiffError> {
    let n = r.u32().map_err(|_| DiffError::Encoding("path"))? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.u32().map_err(|_| DiffError::Encoding("path comp"))? as usize;
        let bytes = r.take(len).map_err(|_| DiffError::Encoding("path bytes"))?;
        out.push(String::from_utf8(bytes.to_vec()).map_err(|_| DiffError::Encoding("path utf8"))?);
    }
    Ok(out)
}

/// Serialize a change set deterministically (buckets sorted by path first).
pub fn serialize_change_set(cs: &ChangeSet) -> Vec<u8> {
    let mut sorted = cs.clone();
    sorted.sort_all();
    let mut out = Vec::new();
    put_bytes(&mut out, &CHANGESET_MAGIC);
    let counts: [&[Modified]; 3] = [
        &sorted.content_modified,
        &sorted.metadata_modified,
        &sorted.type_changed,
    ];
    put_u32(&mut out, sorted.added.len() as u32);
    put_u32(&mut out, sorted.removed.len() as u32);
    for bucket in counts {
        put_u32(&mut out, bucket.len() as u32);
    }
    for a in &sorted.added {
        put_path(&mut out, &a.path);
        put_state(&mut out, &a.state);
    }
    for rm in &sorted.removed {
        put_path(&mut out, &rm.path);
        put_state(&mut out, &rm.state);
    }
    for bucket in counts.iter().copied() {
        for m in bucket {
            put_path(&mut out, &m.path);
            put_state(&mut out, &m.before);
            put_state(&mut out, &m.after);
        }
    }
    out
}

/// Parse a change set produced by [`serialize_change_set`].
pub fn parse_change_set(bytes: &[u8]) -> Result<ChangeSet, DiffError> {
    let bad = || DiffError::Encoding("framing");
    let mut r = Reader::new(bytes);
    if r.take(4).map_err(|_| bad())? != CHANGESET_MAGIC {
        return Err(DiffError::Encoding("magic"));
    }
    let n_added = r.u32().map_err(|_| bad())? as usize;
    let n_removed = r.u32().map_err(|_| bad())? as usize;
    let n_content = r.u32().map_err(|_| bad())? as usize;
    let n_meta = r.u32().map_err(|_| bad())? as usize;
    let n_type = r.u32().map_err(|_| bad())? as usize;

    let mut cs = ChangeSet::default();
    for _ in 0..n_added {
        let path = parse_path(&mut r)?;
        cs.added.push(Added {
            path,
            state: parse_state(&mut r)?,
        });
    }
    for _ in 0..n_removed {
        let path = parse_path(&mut r)?;
        cs.removed.push(Removed {
            path,
            state: parse_state(&mut r)?,
        });
    }
    for _ in 0..n_content {
        let path = parse_path(&mut r)?;
        let before = parse_state(&mut r)?;
        let after = parse_state(&mut r)?;
        cs.content_modified.push(Modified {
            path,
            before,
            after,
        });
    }
    for _ in 0..n_meta {
        let path = parse_path(&mut r)?;
        let before = parse_state(&mut r)?;
        let after = parse_state(&mut r)?;
        cs.metadata_modified.push(Modified {
            path,
            before,
            after,
        });
    }
    for _ in 0..n_type {
        let path = parse_path(&mut r)?;
        let before = parse_state(&mut r)?;
        let after = parse_state(&mut r)?;
        cs.type_changed.push(Modified {
            path,
            before,
            after,
        });
    }
    r.expect_end().map_err(|_| bad())?;
    Ok(cs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PassthroughCipher;
    use crate::manifest::{file_entry, parse_manifest, symlink_entry};
    use crate::snapshot::snapshot_dir;
    use crate::snapshot::testutil::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashSet;

    fn put_tree(store: &Store, node: &TreeNode) -> BlobId {
        store
            .put_meta(
                BlobKind::TreeNode,
                &crate::manifest::serialize_tree_node(node),
            )
            .unwrap()
    }

    /// A component vector becomes a real path under the fixture tree.
    fn disk_path(tree: &std::path::Path, parts: &[String]) -> std::path::PathBuf {
        let mut pb = tree.to_path_buf();
        for c in parts {
            pb.push(c);
        }
        pb
    }

    fn paths_of<'a, T>(items: &'a [T], get: impl Fn(&'a T) -> &'a CompPath) -> Vec<&'a [String]> {
        let mut v: Vec<&[String]> = items.iter().map(|i| get(i).as_slice()).collect();
        v.sort();
        v
    }

    fn one_file(name: &str, exec: bool, sec: i64, nsec: u32, id: u8, len: u64) -> TreeEntry {
        file_entry(name, exec, sec, nsec, vec![([id; 32], len)])
    }

    #[test]
    fn classification_unit_content_metadata_and_type_changes() {
        let (_dir, store) = fresh_store();

        let id_a = put_tree(
            &store,
            &TreeNode {
                entries: vec![one_file("f", false, 0, 0, 1, 10)],
            },
        );
        let id_old = put_tree(
            &store,
            &TreeNode {
                entries: vec![one_file("g", false, 0, 0, 2, 5)],
            },
        );
        let id_new = put_tree(
            &store,
            &TreeNode {
                entries: vec![one_file("g", false, 0, 0, 3, 5)],
            },
        );
        let id_b = put_tree(&store, &TreeNode { entries: vec![] });

        let older = TreeNode {
            entries: vec![
                crate::manifest::dir_entry("dir_same", 0, 0, id_a),
                crate::manifest::dir_entry("dir_chg", 0, 0, id_old),
                one_file("f_cont", false, 1, 0, 1, 10),
                one_file("f_exec", false, 1, 0, 1, 10),
                one_file("f_mt", false, 1, 0, 1, 10),
                symlink_entry("sym", 1, 0, "t1"),
                one_file("tf", false, 0, 0, 4, 3),
                symlink_entry("ts", 0, 0, "old"),
            ],
        };
        let newer = TreeNode {
            entries: vec![
                crate::manifest::dir_entry("dir_same", 0, 0, id_a),
                crate::manifest::dir_entry("dir_chg", 9, 9, id_new),
                one_file("f_cont", false, 1, 0, 9, 10),
                one_file("f_exec", true, 1, 0, 1, 10),
                one_file("f_mt", false, 2, 0, 1, 10),
                symlink_entry("sym", 1, 0, "t2"),
                crate::manifest::dir_entry("tf", 0, 0, id_b),
                one_file("ts", false, 0, 0, 5, 1),
            ],
        };

        let older_id = put_tree(&store, &older);
        let newer_id = put_tree(&store, &newer);
        let cs = diff_roots(&store, &older_id, &newer_id).unwrap();

        assert!(cs.added.is_empty() && cs.removed.is_empty());
        // Equal child ids prune: dir_same appears nowhere.
        assert_eq!(
            paths_of(&cs.content_modified, |m| &m.path),
            [
                ["dir_chg".to_string(), "g".to_string()].as_slice(),
                ["f_cont".to_string()].as_slice(),
                ["sym".to_string()].as_slice(),
            ]
        );
        assert_eq!(
            paths_of(&cs.metadata_modified, |m| &m.path),
            [
                ["f_exec".to_string()].as_slice(),
                ["f_mt".to_string()].as_slice(),
            ]
        );
        assert_eq!(
            paths_of(&cs.type_changed, |m| &m.path),
            [["tf".to_string()].as_slice(), ["ts".to_string()].as_slice(),]
        );

        // Exec flip carries identical chunk lists on both sides.
        let ex = cs
            .metadata_modified
            .iter()
            .find(|m| m.path == ["f_exec"])
            .unwrap();
        assert_eq!(ex.before.chunks, ex.after.chunks);
        assert!(!ex.before.exec && ex.after.exec);

        // Symlink retarget is content (the target IS the content).
        let sy = cs
            .content_modified
            .iter()
            .find(|m| m.path == ["sym"])
            .unwrap();
        assert_eq!(sy.before.target.as_deref(), Some("t1"));
        assert_eq!(sy.after.target.as_deref(), Some("t2"));

        // Type change carries both full states.
        let tf = cs.type_changed.iter().find(|m| m.path == ["tf"]).unwrap();
        assert_eq!(tf.before.kind, EntryKind::File);
        assert_eq!(tf.after.kind, EntryKind::Dir);
    }

    #[test]
    fn identical_manifests_diff_to_empty_via_root_short_circuit() {
        let (_dir, store) = fresh_store();
        let tree_id = put_tree(
            &store,
            &TreeNode {
                entries: vec![one_file("x", false, 1, 2, 3, 4)],
            },
        );
        let base = RootManifest {
            folder_id: [1; 16],
            device_id: [2; 32],
            created_sec: 100,
            created_nsec: 0,
            root_tree_id: tree_id,
            parent_manifest_id: [0; 32],
        };
        assert!(diff_manifests(&store, &base, &base).unwrap().is_empty());

        // Different lineage and timestamps but SAME tree: still empty, via
        // the equal-root early exit.
        let mut other = base.clone();
        other.created_sec = 999;
        other.parent_manifest_id = [7; 32];
        assert!(diff_manifests(&store, &base, &other).unwrap().is_empty());

        // A different tree under the same lineage does produce changes.
        let mut changed_tree = TreeNode {
            entries: vec![one_file("x", false, 1, 2, 3, 4)],
        };
        changed_tree.entries.push(one_file("y", false, 0, 0, 5, 6));
        let mut other = base.clone();
        other.root_tree_id = put_tree(&store, &changed_tree);
        let cs = diff_manifests(&store, &base, &other).unwrap();
        assert_eq!(
            paths_of(&cs.added, |a| &a.path),
            [["y".to_string()].as_slice()]
        );
    }

    /// The ticket's headline acceptance path: snapshot a real directory,
    /// apply a scripted set of mutations, resnapshot, and require the diff
    /// to contain EXACTLY those mutations — untouched sibling subtrees must
    /// appear nowhere.
    #[test]
    fn snapshot_mutate_resnapshot_diff_shows_exactly_the_mutations() {
        use crate::chunker::MIN_SIZE;

        let (dir, store) = fresh_store();
        let tree = dir.path().join("t");
        let mt = (1_700_000_000, 111);

        write_file(&tree.join("keep.txt"), b"stable", false, mt);
        write_file(&tree.join("sub/inner.txt"), b"nested stable", false, mt);
        write_file(&tree.join("gone.txt"), b"bye", false, mt);
        write_file(&tree.join("edit.txt"), b"version one", false, mt);
        write_file(&tree.join("touch.txt"), b"same bytes", false, mt);
        write_file(&tree.join("script.sh"), b"#!/bin/sh\nexit 0\n", false, mt);
        write_file(
            &tree.join("big.bin"),
            &prng(9, MIN_SIZE * 24 + 999_983),
            false,
            mt,
        );
        std::fs::create_dir_all(tree.join("olddir")).unwrap();
        set_dir_mtime(&tree.join("olddir"), mt);
        set_dir_mtime(&tree.join("sub"), mt);
        std::os::unix::fs::symlink("sub/inner.txt", tree.join("link")).unwrap();

        let poly = poly_of(21);
        let idn = identity((10, 20));
        let s1 = snapshot_dir(&store, poly, &tree, &idn).unwrap();

        // ---- scripted mutations ----
        std::fs::remove_file(tree.join("gone.txt")).unwrap(); // delete file
        std::fs::remove_dir(tree.join("olddir")).unwrap(); // remove empty dir
        write_file(
            &tree.join("edit.txt"),
            b"version two!!",
            false,
            (mt.0 + 1, mt.1),
        );
        // append to the big multi-chunk file (log-style tail append)
        let extra_len = 300_000usize;
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(tree.join("big.bin"))
                .unwrap();
            f.write_all(&prng(10, extra_len)).unwrap();
        }
        // flip exec bit, bytes untouched
        let mut perm = std::fs::metadata(tree.join("script.sh"))
            .unwrap()
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perm.set_mode(0o755);
        std::fs::set_permissions(tree.join("script.sh"), perm).unwrap();
        // swap symlink target
        std::fs::remove_file(tree.join("link")).unwrap();
        std::os::unix::fs::symlink("elsewhere/b.txt", tree.join("link")).unwrap();
        // rewrite with byte-identical content, bump only mtime
        write_file(
            &tree.join("touch.txt"),
            b"same bytes",
            false,
            (mt.0 + 9, 222),
        );
        // additions: a bare file and a new dir holding a file
        write_file(&tree.join("added.txt"), b"new kid", false, (mt.0 + 2, 5));
        write_file(
            &tree.join("newdir/inside.txt"),
            b"deep",
            false,
            (mt.0 + 3, 6),
        );
        set_dir_mtime(&tree.join("newdir"), (mt.0 + 4, 7));

        let s2 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
        let cs = diff_manifests(&store, &s1.manifest, &s2.manifest).unwrap();

        assert_eq!(
            paths_of(&cs.added, |a| &a.path),
            [
                ["added.txt".to_string()].as_slice(),
                ["newdir".to_string()].as_slice(),
                ["newdir".to_string(), "inside.txt".to_string()].as_slice(),
            ]
        );
        assert_eq!(
            paths_of(&cs.removed, |r| &r.path),
            [
                ["gone.txt".to_string()].as_slice(),
                ["olddir".to_string()].as_slice(),
            ]
        );
        assert_eq!(
            paths_of(&cs.content_modified, |m| &m.path),
            [
                ["big.bin".to_string()].as_slice(),
                ["edit.txt".to_string()].as_slice(),
                ["link".to_string()].as_slice(),
            ]
        );
        assert_eq!(
            paths_of(&cs.metadata_modified, |m| &m.path),
            [
                ["script.sh".to_string()].as_slice(),
                ["touch.txt".to_string()].as_slice(),
            ]
        );
        assert!(cs.type_changed.is_empty());

        // Untouched sibling subtrees appear NOWHERE in any bucket.
        let every: Vec<&CompPath> = cs
            .added
            .iter()
            .map(|a| &a.path)
            .chain(cs.removed.iter().map(|r| &r.path))
            .chain(cs.content_modified.iter().map(|m| &m.path))
            .chain(cs.metadata_modified.iter().map(|m| &m.path))
            .chain(cs.type_changed.iter().map(|m| &m.path))
            .collect();
        for p in every {
            assert!(
                p.as_slice() != ["keep.txt".to_string()]
                    && p.first().map(String::as_str) != Some("sub"),
                "untouched path leaked into diff: {p:?}"
            );
        }

        // Exec flip: content identical, flag flipped.
        let sh = cs
            .metadata_modified
            .iter()
            .find(|m| m.path == ["script.sh"])
            .unwrap();
        assert_eq!(sh.before.chunks, sh.after.chunks);
        assert!(!sh.before.exec && sh.after.exec);

        // Touch-only: chunks identical, mtime moved.
        let tc = cs
            .metadata_modified
            .iter()
            .find(|m| m.path == ["touch.txt"])
            .unwrap();
        assert_eq!(tc.before.chunks, tc.after.chunks);
        assert_ne!(
            (tc.before.mtime_sec, tc.before.mtime_nsec),
            (tc.after.mtime_sec, tc.after.mtime_nsec)
        );

        // CDC stability end-to-end: appending at the tail preserves every
        // earlier boundary, so all but the final old chunk are shared, in
        // order, as a prefix of the new list.
        let big = cs
            .content_modified
            .iter()
            .find(|m| m.path == ["big.bin"])
            .unwrap();
        let before_chunks = &big.before.chunks;
        let after_chunks = &big.after.chunks;
        assert!(before_chunks.len() >= 5, "fixture must be multi-chunk");
        assert!(after_chunks.len() >= before_chunks.len());
        let shared = before_chunks.len() - 1;
        assert_eq!(
            before_chunks[..shared],
            after_chunks[..shared],
            "leading unchanged chunks must survive the append"
        );
        let before_bytes: u64 = before_chunks.iter().map(|c| c.1).sum();
        let after_bytes: u64 = after_chunks.iter().map(|c| c.1).sum();
        assert_eq!(after_bytes - before_bytes, extra_len as u64);

        // The change set alone tells a materializer what to fetch: the edit
        // entry exposes the NEW chunk ids and mode/exec flag.
        let ed = cs
            .content_modified
            .iter()
            .find(|m| m.path == ["edit.txt"])
            .unwrap();
        assert_eq!(ed.after.kind, EntryKind::File);
        assert_eq!(ed.after.chunks.len(), 1);
        let stored = store
            .get(BlobKind::DataChunk, &ed.after.chunks[0].0)
            .unwrap();
        assert_eq!(stored, b"version two!!");
    }

    /// Proof that diff never touches file bytes: drop the source directory
    /// AND delete every pack containing data chunks, then diff from a freshly
    /// opened store that can only reach metadata blobs.
    #[test]
    fn diff_reads_no_file_bytes_survives_deleted_sources_and_data_packs() {
        let (dir, store) = fresh_store();
        let tree = dir.path().join("t");
        write_file(&tree.join("base.txt"), b"v1", false, (1, 0));
        write_file(&tree.join("other.bin"), b"static", true, (2, 0));

        let poly = poly_of(31);
        let idn = identity((1, 1));
        let s1 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
        write_file(
            &tree.join("base.txt"),
            b"v2 completely different",
            false,
            (3, 0),
        );
        let s2 = snapshot_dir(&store, poly, &tree, &idn).unwrap();

        // Every pack holding data chunks is doomed; metadata packs survive.
        let packs_dir = store.packs_dir();
        let mut doomed = Vec::new();
        for e in std::fs::read_dir(&packs_dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid: [u8; 32] =
                crate::format::unhex(name.trim_end_matches(".pack")).expect("hex pack name");
            let (_, footer) = store.pack_blob_list(&pid).unwrap();
            if footer.iter().any(|f| f.kind == BlobKind::DataChunk) {
                doomed.push(name);
            }
        }
        assert!(!doomed.is_empty(), "expected at least one data pack");

        let m1 = parse_manifest(&store.get(BlobKind::Manifest, &s1.manifest_id).unwrap()).unwrap();
        let m2 = parse_manifest(&store.get(BlobKind::Manifest, &s2.manifest_id).unwrap()).unwrap();
        // Persist the index so the reopened store can find metadata blobs.
        store.write_index_snapshot().unwrap();
        drop(store);
        for name in &doomed {
            std::fs::remove_file(packs_dir.join(name)).unwrap();
        }
        // The source tree ceases to exist entirely.
        std::fs::remove_dir_all(&tree).unwrap();

        let fresh = Store::open(dir.path(), [0u8; 32], Box::new(PassthroughCipher)).unwrap();
        let cs = diff_manifests(&fresh, &m1, &m2).unwrap();
        assert_eq!(
            paths_of(&cs.content_modified, |m| &m.path),
            [["base.txt".to_string()].as_slice()]
        );
        assert!(cs.added.is_empty());
        assert!(cs.removed.is_empty());
        assert!(cs.metadata_modified.is_empty());
        assert!(cs.type_changed.is_empty());
    }

    #[test]
    fn change_set_codec_round_trips_every_bucket() {
        let state = |kind: EntryKind, exec: bool, target: Option<&str>| EntryState {
            kind,
            exec,
            mtime_sec: -42,
            mtime_nsec: 123_456,
            chunks: if kind == EntryKind::File {
                vec![([1u8; 32], 7), ([2u8; 32], 900)]
            } else {
                vec![]
            },
            target: target.map(str::to_string),
        };
        let mut cs = ChangeSet {
            added: vec![Added {
                path: vec!["nouveau".to_string(), "caf\u{e9}".to_string()],
                state: state(EntryKind::File, true, None),
            }],
            removed: vec![Removed {
                path: vec!["vieux".to_string()],
                state: state(EntryKind::Dir, false, None),
            }],
            content_modified: vec![Modified {
                path: vec!["a".to_string(), "b".to_string()],
                before: state(EntryKind::Symlink, false, Some("old")),
                after: state(EntryKind::Symlink, false, Some("new/er")),
            }],
            metadata_modified: vec![Modified {
                path: vec!["z".to_string()],
                before: state(EntryKind::File, false, None),
                after: state(EntryKind::File, true, None),
            }],
            type_changed: vec![Modified {
                path: vec!["flip".to_string()],
                before: state(EntryKind::File, false, None),
                after: state(EntryKind::Dir, false, None),
            }],
        };

        let bytes = serialize_change_set(&cs);
        let back = parse_change_set(&bytes).unwrap();
        assert_eq!(back, cs);

        // Deterministic encoding independent of insertion order.
        cs.added.reverse();
        assert_eq!(serialize_change_set(&cs), bytes);

        // Truncation is refused, not guessed at.
        assert!(parse_change_set(&bytes[..bytes.len() - 3]).is_err());
        assert!(parse_change_set(b"XXXXrest").is_err());
    }

    // --- seeded fuzz-lite -------------------------------------------------

    /// (bytes, exec, mtime) for one modeled file.
    type FileSpec = (Vec<u8>, bool, (i64, u32));
    type BucketSet = HashSet<Vec<String>>;

    #[derive(Clone, Default)]
    struct Model {
        files: std::collections::BTreeMap<Vec<String>, FileSpec>,
        dirs: HashSet<Vec<String>>,
    }

    const WORDS: [&str; 5] = ["alpha", "beta", "gamma", "delta", "zeta"];

    impl Model {
        fn random_dir<R: Rng>(&self, rng: &mut R) -> Vec<String> {
            if !self.dirs.is_empty() && rng.gen_bool(0.5) {
                let i = rng.gen::<usize>() % self.dirs.len();
                self.dirs.iter().nth(i).unwrap().clone()
            } else {
                Vec::new()
            }
        }

        /// Register every strict ancestor so added dirs appear in the model.
        fn register_dirs(&mut self, path: &[String]) {
            for k in 1..path.len() {
                self.dirs.insert(path[..k].to_vec());
            }
        }

        fn add_random<R: Rng>(&mut self, rng: &mut R, tree: &std::path::Path) {
            let base = self.random_dir(rng);
            if rng.gen_bool(0.3) {
                let mut d = base;
                d.push(WORDS[rng.gen::<usize>() % WORDS.len()].to_string());
                if self.dirs.contains(&d) {
                    return;
                }
                self.register_dirs(&d);
                self.dirs.insert(d.clone());
                std::fs::create_dir_all(disk_path(tree, &d)).unwrap();
                set_dir_mtime(&disk_path(tree, &d), (1_700_000_000, 1));
                return;
            }
            let mut p = base;
            p.push(format!("{}.bin", WORDS[rng.gen::<usize>() % WORDS.len()]));
            if self.files.contains_key(&p) {
                return;
            }
            self.register_dirs(&p);
            let bytes: Vec<u8> = (0..rng.gen::<usize>() % 4000).map(|_| rng.gen()).collect();
            let exec = rng.gen_bool(0.3);
            let mt = (
                1_700_000_000 + rng.gen::<i64>().rem_euclid(1000),
                rng.gen::<u32>() % 1_000_000_000,
            );
            write_file(&disk_path(tree, &p), &bytes, exec, mt);
            self.files.insert(p, (bytes, exec, mt));
        }

        fn mutate_random<R: Rng>(&mut self, rng: &mut R, tree: &std::path::Path) {
            if self.files.is_empty() {
                self.add_random(rng, tree);
                return;
            }
            let i = rng.gen::<usize>() % self.files.len();
            let path = self.files.keys().nth(i).unwrap().clone();
            match rng.gen::<u8>() % 4 {
                0 => {
                    // New content (and a new mtime): ContentModified.
                    let bytes: Vec<u8> =
                        (0..rng.gen::<usize>() % 4000).map(|_| rng.gen()).collect();
                    let mt = (
                        1_700_000_500 + rng.gen::<i64>().rem_euclid(1000),
                        rng.gen::<u32>() % 1_000_000_000,
                    );
                    let exec = self.files[&path].1;
                    write_file(&disk_path(tree, &path), &bytes, exec, mt);
                    let slot = self.files.get_mut(&path).unwrap();
                    slot.0 = bytes;
                    slot.2 = mt;
                }
                1 => {
                    // Flip exec, keep bytes and mtime: MetadataModified.
                    let (b, e, m) = self.files[&path].clone();
                    write_file(&disk_path(tree, &path), &b, !e, m);
                    self.files.get_mut(&path).unwrap().1 = !e;
                }
                2 => {
                    // Bump mtime only: MetadataModified.
                    let (b, e, m) = self.files[&path].clone();
                    let mt = (m.0 + 5, m.1 ^ 777);
                    write_file(&disk_path(tree, &path), &b, e, mt);
                    self.files.get_mut(&path).unwrap().2 = mt;
                }
                _ => {
                    std::fs::remove_file(disk_path(tree, &path)).unwrap();
                    self.files.remove(&path);
                }
            }
        }
    }

    fn expected_paths(
        before: &Model,
        after: &Model,
    ) -> (BucketSet, BucketSet, BucketSet, BucketSet) {
        let mut added = HashSet::new();
        let mut removed = HashSet::new();
        for d in &after.dirs {
            if !before.dirs.contains(d) {
                added.insert(d.clone());
            }
        }
        for d in &before.dirs {
            if !after.dirs.contains(d) {
                removed.insert(d.clone());
            }
        }
        for f in after.files.keys() {
            if !before.files.contains_key(f) {
                added.insert(f.clone());
            }
        }
        for f in before.files.keys() {
            if !after.files.contains_key(f) {
                removed.insert(f.clone());
            }
        }
        let mut content = HashSet::new();
        let mut meta = HashSet::new();
        for (p, (b, e, m)) in &before.files {
            if let Some(a) = after.files.get(p) {
                if &a.0 != b {
                    content.insert(p.clone());
                } else if &a.1 != e || &a.2 != m {
                    meta.insert(p.clone());
                }
            }
        }
        (added, removed, content, meta)
    }

    #[test]
    fn fuzz_seeded_trees_resnapshot_identical_and_match_model_oracle() {
        for seed in 0u64..6 {
            let mut rng = StdRng::seed_from_u64(seed * 7919 + 13);
            let (_dir, store) = fresh_store();
            let tree = _dir.path().join("t");

            let mut model = Model::default();
            for _ in 0..(4 + rng.gen::<usize>() % 4) {
                model.add_random(&mut rng, &tree);
            }

            let poly = poly_of(seed + 100);
            let idn = identity((3, 4));
            let s1 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
            let s2 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
            assert_eq!(s1.root_tree_id, s2.root_tree_id, "seed {seed}");
            assert!(
                diff_manifests(&store, &s1.manifest, &s2.manifest)
                    .unwrap()
                    .is_empty(),
                "unchanged resnapshot must diff empty (seed {seed})"
            );

            let before = model.clone();
            model.mutate_random(&mut rng, &tree);
            let s3 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
            let cs = diff_manifests(&store, &s2.manifest, &s3.manifest).unwrap();

            let (ea, er, ec, em) = expected_paths(&before, &model);
            let act_added: HashSet<Vec<String>> = cs.added.iter().map(|a| a.path.clone()).collect();
            let act_removed: HashSet<Vec<String>> =
                cs.removed.iter().map(|r| r.path.clone()).collect();
            let act_content: HashSet<Vec<String>> =
                cs.content_modified.iter().map(|m| m.path.clone()).collect();
            let act_meta: HashSet<Vec<String>> = cs
                .metadata_modified
                .iter()
                .map(|m| m.path.clone())
                .collect();
            assert_eq!(act_added, ea, "added (seed {seed})");
            assert_eq!(act_removed, er, "removed (seed {seed})");
            assert_eq!(act_content, ec, "content (seed {seed})");
            assert_eq!(act_meta, em, "metadata (seed {seed})");
            assert!(cs.type_changed.is_empty(), "seed {seed}");
        }
    }
}
