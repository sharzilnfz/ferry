//! Incremental passes (T-004): rebuild dirty subtrees, splice them into the
//! cached tree, and emit a manifest identical to a from-scratch
//! [`ferry_store::snapshot::snapshot_dir`] result.
//!
//! How a pass works, in order:
//!
//! 1. The caller supplies a **transitivity-closed** dirty set: every marked
//!    directory plus all of its ancestors up to the root (see
//!    [`close_under_ancestors`]). Closure is what lets child-id changes flow
//!    upward without a second pass.
//! 2. Directories are rebuilt deepest-first. Rebuilding a directory lists it
//!    on disk; for each entry:
//!     - unchanged files are detected by a **size/mtime/exec short-circuit**
//!       against the cached entry and reuse their chunk list without a single
//!       byte read (`PassStats::bytes_chunked` counts only bytes actually
//!       read+chunked — the hasher hook used by tests and the benchmark),
//!     - changed/new files are read, CDC-chunked through the folder's chunker,
//!       and stored like `snapshot_dir` does,
//!     - untouched subdirectories keep their cached node and id wholesale —
//!       not even a stat is spent on their contents,
//!     - symlinks are re-read (cheap) and NFC/UTF-8 rules applied.
//! 3. Walk rules mirror `snapshot.rs` exactly: NFC names, loud refusals for
//!     non-UTF-8 names/targets and unsupported file types, sibling-collision
//!     hard errors, exec-bit-only permissions. One documented divergence:
//!     an entry that vanishes mid-pass is skipped rather than failed — the
//!     next event or audit repairs it, and racing deletions are not a scan
//!     bug.
//! 4. The new root id is compared with the previous one. Only a CHANGED root
//!     produces a manifest (parent = previous manifest id), so no-op bursts
//!     write zero pack bytes.
//!
//! Cache hygiene: when a rebuilt listing lacks a previously cached child
//! directory, that whole prefix is dropped from the cache so deleted or
//! renamed-away subtrees can never satisfy later short-circuits.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use ferry_store::manifest::{
    dir_entry, file_entry, serialize_manifest, serialize_tree_node, symlink_entry, EntryPayload,
    RootManifest, TreeEntry, TreeNode,
};
use ferry_store::snapshot::{ensure_no_sibling_collisions, RefusedPath, RefusalReason};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

use crate::error::ScanError;
use crate::ignore::IgnorePolicy;
use crate::policy::{RelPath, Trigger};
use crate::state::{CachedDir, DirCache};

/// Exec-bit detection, same constant as ferry-store's snapshot walker.
const EXEC_MODE_BITS: u32 = 0o111;

/// Counters describing one completed pass. `bytes_chunked` is the
/// short-circuit proof hook: a pass over an untouched tree must report 0.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    pub trigger: Trigger,
    /// Entries present in the resulting tree.
    pub files: usize,
    pub dirs: usize,
    pub symlinks: usize,
    /// Bytes actually read from disk and pushed through the chunker this
    /// pass (0 when everything short-circuited).
    pub bytes_chunked: u64,
    /// Files whose bytes were re-hashed (= re-chunked).
    pub files_rehashed: usize,
    /// Dirty directories rebuilt.
    pub dirty_dirs: usize,
}

/// Result of one incremental pass whose root changed.
#[derive(Clone, Debug)]
pub struct ScanOutput {
    pub manifest: RootManifest,
    pub manifest_id: BlobId,
    pub root_tree_id: BlobId,
    pub stats: PassStats,
    pub refused: Vec<RefusedPath>,
}

/// Close a set of marked directories under "all ancestors plus root", as the
/// pass requires. Order-stable and deterministic.
pub(crate) fn close_under_ancestors(dirs: &[RelPath]) -> BTreeSet<RelPath> {
    let mut out = BTreeSet::new();
    for d in dirs {
        let mut anc: RelPath = Vec::new();
        out.insert(anc.clone());
        for c in d {
            anc.push(c.clone());
            out.insert(anc.clone());
        }
    }
    out
}

/// Everything one pass needs. Owns scratch state; the cache is borrowed
/// mutably and left coherent for the next pass.
pub(crate) struct Walker<'a> {
    store: &'a Store,
    poly: u64,
    ignore: &'a dyn IgnorePolicy,
    disk_root: &'a Path,
    cache: &'a mut DirCache,

    stats: PassStats,
    refused: Vec<RefusedPath>,
    /// rel -> freshly rebuilt node id for this pass.
    rebuilt: HashMap<RelPath, BlobId>,
}

impl<'a> Walker<'a> {
    /// Run one incremental pass. Returns `None` when nothing below the root
    /// changed (root tree id identical to `prev_root_tree_id`); stats are
    /// still reported either way via `stats_out`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        store: &'a Store,
        poly: u64,
        ignore: &'a dyn IgnorePolicy,
        disk_root: &'a Path,
        cache: &'a mut DirCache,
        dirty_closed: &BTreeSet<RelPath>,
        trigger: Trigger,
        identity: &ferry_store::snapshot::SnapshotIdentity,
        prev_root_tree_id: BlobId,
        stats_out: &mut PassStats,
    ) -> Result<Option<ScanOutput>, ScanError> {
        // Deepest-first: every dirty descendant is rebuilt before any of its
        // ancestors needs its id.
        let mut order: Vec<&RelPath> = dirty_closed.iter().collect();
        order.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

        let mut w = Walker {
            store,
            poly,
            ignore,
            disk_root,
            cache,
            stats: PassStats {
                trigger,
                ..PassStats::default()
            },
            refused: Vec::new(),
            rebuilt: HashMap::new(),
        };

        for d in order {
            if w.rebuilt.contains_key(d) {
                continue;
            }
            // A vanished dirty directory is simply absent; its parent's
            // rebuild omits it. The root itself vanishing is handled by the
            // engine before a pass ever runs.
            if d.is_empty() && !w.disk_root.is_dir() {
                return Err(ScanError::Watch("watched root is not a directory".into()));
            }
            w.rebuild_dir(d, dirty_closed)?;
        }

        let root_id = *w.rebuilt.get(&Vec::new()).ok_or_else(|| {
            ScanError::Watch("dirty set was not closed under ancestry (no root)".into())
        })?;

        w.stats.dirty_dirs = w.rebuilt.len();
        *stats_out = w.stats.clone();

        if root_id == prev_root_tree_id {
            return Ok(None);
        }

        let manifest = RootManifest {
            folder_id: identity.folder_id,
            device_id: identity.device_id,
            created_sec: identity.created_sec,
            created_nsec: identity.created_nsec,
            root_tree_id: root_id,
            parent_manifest_id: identity.parent_manifest_id,
        };
        let manifest_bytes = serialize_manifest(&manifest);
        let manifest_id = w.store.put_meta(BlobKind::Manifest, &manifest_bytes)?;
        w.store.flush()?;

        Ok(Some(ScanOutput {
            manifest,
            manifest_id,
            root_tree_id: root_id,
            stats: w.stats,
            refused: w.refused,
        }))
    }

    fn io_err(path: &Path) -> impl Fn(std::io::Error) -> ScanError + '_ {
        let path = path.to_path_buf();
        move |source| ScanError::Io {
            path: path.clone(),
            source,
        }
    }

    /// List `rel` on disk, build its TreeNode reusing cache where valid,
    /// store it, update the cache, prune stale children. Returns the node id.
    fn rebuild_dir(
        &mut self,
        rel: &RelPath,
        dirty_closed: &BTreeSet<RelPath>,
    ) -> Result<BlobId, ScanError> {
        let disk = self.disk_path(rel);

        let mut names: Vec<std::ffi::OsString> = Vec::new();
        match std::fs::read_dir(&disk) {
            Ok(rd) => {
                for entry in rd {
                    let entry = entry.map_err(Self::io_err(&disk))?;
                    names.push(entry.file_name());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Vanished mid-pass (or between event and pass): record an
                // empty listing so parents splicing this pass see absence;
                // the parent's fresh listing will omit (and prune) it.
                self.cache.remove_prefix(rel);
                let empty_bytes = serialize_tree_node(&TreeNode { entries: vec![] });
                let empty = CachedDir {
                    id: *blake3::hash(&empty_bytes).as_bytes(),
                    node: TreeNode { entries: vec![] },
                };
                self.cache.insert(rel.clone(), empty);
                let bytes = serialize_tree_node(&TreeNode { entries: vec![] });
                let id = self.store.put_meta(BlobKind::TreeNode, &bytes)?;
                self.rebuilt.insert(rel.clone(), id);
                return Ok(id);
            }
            Err(e) => return Err(Self::io_err(&disk)(e)),
        }
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        let old_node: Option<TreeNode> = self.cache.node(rel).map(|c| c.node.clone());
        let mut entries: Vec<TreeEntry> = Vec::with_capacity(names.len());
        let mut listed_dirs: Vec<RelPath> = Vec::new();

        for name in names {
            let raw = name.as_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            let component = match std::str::from_utf8(raw) {
                Ok(s) => s.nfc().collect::<String>(),
                Err(_) => {
                    self.refused.push(RefusedPath {
                        path: vec![String::from_utf8_lossy(raw).into_owned()],
                        reason: RefusalReason::NonUtf8Name,
                    });
                    continue;
                }
            };
            let mut child_rel = rel.clone();
            child_rel.push(component.clone());
            if self.ignore.ignored(&child_rel) {
                continue;
            }
            let child_disk = disk.join(&name);

            let meta = match std::fs::symlink_metadata(&child_disk) {
                Ok(m) => m,
                // Vanished mid-pass: skip; next event or audit repairs.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Self::io_err(&child_disk)(e)),
            };
            let ft = meta.file_type();

            let entry = if ft.is_symlink() {
                let target = std::fs::read_link(&child_disk).map_err(Self::io_err(&child_disk))?;
                match target.to_str() {
                    // Stored verbatim apart from UTF-8 validation, byte-for-
                    // byte identical to the snapshot_dir oracle (T-003).
                    Some(t) => {
                        self.stats.symlinks += 1;
                        symlink_entry(&component, mtime_sec(&meta), mtime_nsec(&meta), t)
                    }
                    None => {
                        self.refused.push(RefusedPath {
                            path: child_rel,
                            reason: RefusalReason::NonUtf8SymlinkTarget,
                        });
                        continue;
                    }
                }
            } else if ft.is_dir() {
                let child_id = self.ensure_child(&child_rel, &disk, dirty_closed)?;
                listed_dirs.push(child_rel.clone());
                self.stats.dirs += 1;
                dir_entry(&component, mtime_sec(&meta), mtime_nsec(&meta), child_id)
            } else if ft.is_file() {
                let size = meta.len();
                let mt = (mtime_sec(&meta), mtime_nsec(&meta));
                let exec = meta.permissions().mode() & EXEC_MODE_BITS != 0;
                self.stats.files += 1;

                let chunks = match old_node.as_ref().and_then(|n| find_entry(n, &component)) {
                    Some(prev) if reusable(prev, size, mt, exec) => {
                        match &prev.payload {
                            EntryPayload::File { chunks, .. } => chunks.clone(),
                            _ => unreachable!("reusable() guarantees a File payload"),
                        }
                        // Short-circuit hit: zero bytes read.
                    }
                    _ => {
                        let bytes =
                            std::fs::read(&child_disk).map_err(Self::io_err(&child_disk))?;
                        let mut chunks = Vec::new();
                        for piece in ferry_store::chunker::chunk(self.poly, &bytes) {
                            let id = self.store.put_data(piece)?;
                            self.stats.bytes_chunked += piece.len() as u64;
                            chunks.push((id, piece.len() as u64));
                        }
                        self.stats.files_rehashed += 1;
                        chunks
                    }
                };
                file_entry(&component, exec, mt.0, mt.1, chunks)
            } else {
                self.refused.push(RefusedPath {
                    path: child_rel,
                    reason: RefusalReason::UnknownFileType,
                });
                continue;
            };
            entries.push(entry);
        }

        ensure_no_sibling_collisions(rel, &entries).map_err(|e| match e {
            ferry_store::snapshot::SnapshotError::NameCollision { parent, name } => {
                ScanError::NameCollision { parent, name }
            }
            other => ScanError::Snapshot(other),
        })?;

        // Cache hygiene: children absent from the fresh listing were deleted
        // or renamed away — drop their records so no future pass can be
        // fooled into reusing them.
        let stale: Vec<RelPath> = self
            .cache
            .keys_under(rel)
            .filter(|k| !listed_dirs.iter().any(|l| l == *k))
            .cloned()
            .collect();
        for s in stale {
            self.cache.remove_prefix(&s);
        }

        let node = TreeNode { entries };
        let bytes = serialize_tree_node(&node);
        let id = self.store.put_meta(BlobKind::TreeNode, &bytes)?;
        self.cache.insert(rel.clone(), CachedDir { id, node });
        self.rebuilt.insert(rel.clone(), id);
        Ok(id)
    }

    /// Resolve the tree-node id for a child directory encountered while
    /// rebuilding `parent_disk`'s listing.
    fn ensure_child(
        &mut self,
        child_rel: &RelPath,
        _parent_disk: &Path,
        dirty_closed: &BTreeSet<RelPath>,
    ) -> Result<BlobId, ScanError> {
        if let Some(id) = self.rebuilt.get(child_rel) {
            return Ok(*id);
        }
        if dirty_closed.contains(child_rel) {
            // Ordering guarantees deepest-first, but a defensive inline
            // rebuild keeps correctness independent of sort details.
            return self.rebuild_dir(child_rel, dirty_closed);
        }
        if let Some(cached) = self.cache.node(child_rel) {
            return Ok(cached.id);
        }
        // Not cached and not marked: appeared without a matching event (or
        // cache gap). Walk it now — correctness first.
        self.rebuild_dir(child_rel, dirty_closed)
    }

    fn disk_path(&self, rel: &RelPath) -> PathBuf {
        let mut p = self.disk_root.to_path_buf();
        for c in rel {
            p.push(c);
        }
        p
    }
}

fn find_entry<'n>(node: &'n TreeNode, name: &str) -> Option<&'n TreeEntry> {
    node.entries.iter().find(|e| e.name == name)
}

/// The short-circuit predicate: same size, mtime, exec bit, and file-ness
/// means the bytes on disk are assumed identical to the recorded chunk list.
/// Anything else forces a full read+hash.
fn reusable(prev: &TreeEntry, size: u64, mt: (i64, u32), exec: bool) -> bool {
    match &prev.payload {
        EntryPayload::File {
            size: prev_size,
            ..
        } => {
            prev.mtime_sec == mt.0 && prev.mtime_nsec == mt.1 && *prev_size == size && prev.exec == exec
        }
        _ => false,
    }
}

// Metadata accessors mirroring snapshot.rs's unix-only rules. This crate is
// unix-gated the same way ferry-store's snapshot module is.
#[cfg(unix)]
mod meta {
    use std::os::unix::fs::MetadataExt;

    pub(super) fn mtime_sec(meta: &std::fs::Metadata) -> i64 {
        meta.mtime()
    }

    pub(super) fn mtime_nsec(meta: &std::fs::Metadata) -> u32 {
        u32::try_from(meta.mtime_nsec()).expect("mtime nanoseconds out of u32 range")
    }
}
#[cfg(unix)]
use meta::{mtime_nsec, mtime_sec};

#[cfg(not(unix))]
fn mtime_sec(_meta: &std::fs::Metadata) -> i64 {
    0
}
#[cfg(not(unix))]
fn mtime_nsec(_meta: &std::fs::Metadata) -> u32 {
    0
}
