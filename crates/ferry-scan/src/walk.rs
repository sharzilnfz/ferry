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
//!    - untouched subdirectories keep their cached node and id wholesale —
//!      not even a stat is spent on their contents,
//!    - symlinks are re-read (cheap) and NFC/UTF-8 rules applied.
//! 3. Walk rules mirror `snapshot.rs` exactly: NFC names, loud refusals for
//!    non-UTF-8 names/targets and unsupported file types, sibling-collision
//!    hard errors, exec-bit-only permissions. One documented divergence:
//!    an entry that vanishes mid-pass is skipped rather than failed — the
//!    next event or audit repairs it, and racing deletions are not a scan
//!    bug.
//! 4. The new root id is compared with the previous one. Only a CHANGED root
//!    produces a manifest (parent = previous manifest id), so no-op bursts
//!    write zero pack bytes.
//!
//! Cache hygiene: when a rebuilt listing lacks a previously cached child
//! directory, that whole prefix is dropped from the cache so deleted or
//! renamed-away subtrees can never satisfy later short-circuits.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_platform::{classify_link, is_reserved_device_name};
use unicode_normalization::UnicodeNormalization;

use ferry_store::manifest::{
    dir_entry, file_entry, serialize_manifest, serialize_tree_node, symlink_entry, EntryPayload,
    RootManifest, TreeEntry, TreeNode,
};
use ferry_store::snapshot::{
    ensure_no_collisions, RefusalReason, RefusedPath,
};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

use crate::error::ScanError;
use crate::ignore::IgnorePolicy;
use crate::policy::{RelPath, Trigger};
use crate::state::{CachedDir, DirCache};

/// Exec-bit detection, same constant as ferry-store's snapshot walker.
/// (Kept for reference; the live check lives in `live_exec`.)

/// Structural exclusion: the store directory never enters manifests, walks,
/// watches, or sweeps. This is folder-layout contract (`docs/store-format.md`),
/// not an ignore rule — hence hard-coded rather than routed through
/// [`IgnorePolicy`].
pub(crate) fn is_store_component(name: &str) -> bool {
    name == ferry_store::store::STORE_DIR_NAME
}

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
    /// Wall-clock time of the pass itself (walk + hash + store IO),
    /// excluding debounce/event latency.
    pub duration: Duration,
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
        if dirty_closed.is_empty() {
            return Ok(None);
        }
        let mut order: Vec<&RelPath> = dirty_closed.iter().collect();
        order.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

        let started = std::time::Instant::now();
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
        w.stats.duration = started.elapsed();
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

    /// Record an empty listing for `rel`: vanished mid-pass, a file path
    /// mis-marked as dirty, or the structurally excluded store directory.
    /// Parents splicing this pass see absence and omit (and prune) it.
    fn splice_absent(&mut self, rel: &RelPath) -> Result<BlobId, ScanError> {
        self.cache.remove_prefix(rel);
        let empty_bytes = serialize_tree_node(&TreeNode { entries: vec![] });
        let empty = CachedDir {
            id: *blake3::hash(&empty_bytes).as_bytes(),
            node: TreeNode { entries: vec![] },
        };
        self.cache.insert(rel.clone(), empty);
        let id = self.store.put_meta(BlobKind::TreeNode, &empty_bytes)?;
        self.rebuilt.insert(rel.clone(), id);
        Ok(id)
    }

    /// List `rel` on disk, build its TreeNode reusing cache where valid,
    /// store it, update the cache, prune stale children. Returns the node id.
    fn rebuild_dir(
        &mut self,
        rel: &RelPath,
        dirty_closed: &BTreeSet<RelPath>,
    ) -> Result<BlobId, ScanError> {
        // Inside or at the store directory: structurally excluded. Behave
        // exactly like absence so parents never point at it.
        if rel.last().map(|c| is_store_component(c)).unwrap_or(false) {
            return self.splice_absent(rel);
        }
        let disk = self.disk_path(rel);

        let mut names: Vec<std::ffi::OsString> = Vec::new();
        match std::fs::read_dir(&disk) {
            Ok(rd) => {
                for entry in rd {
                    let entry = entry.map_err(Self::io_err(&disk))?;
                    names.push(entry.file_name());
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    // A changed FILE path can land in the dirty set (policy
                    // marks event targets defensively); rebuilding it as a
                    // directory must degrade to absence, not an error.
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                return self.splice_absent(rel);
            }
            Err(e) => return Err(Self::io_err(&disk)(e)),
        }
        names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));

        let old_node: Option<TreeNode> = self.cache.node(rel).map(|c| c.node.clone());
        let mut entries: Vec<TreeEntry> = Vec::with_capacity(names.len());
        let mut listed_dirs: Vec<RelPath> = Vec::new();

        for name in names {
            let raw = name.as_encoded_bytes();
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
            // Structural exclusion: the store directory is not user content.
            if is_store_component(&component) {
                continue;
            }
            let mut child_rel = rel.clone();
            child_rel.push(component.clone());
            if self.ignore.ignored(&child_rel) {
                continue;
            }
            // Reserved Windows device names can never materialize on a
            // Windows endpoint; refuse loudly at the source (T-012).
            if is_reserved_device_name(&component) {
                self.refused.push(RefusedPath {
                    path: child_rel,
                    reason: RefusalReason::ReservedName,
                });
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
                    // T-012 policy: relative internal targets sync as links;
                    // absolute or root-escaping targets are refused loudly.
                    Some(t) => match classify_link(rel.len(), t) {
                        ferry_platform::LinkDecision::SyncAsLink => {
                            self.stats.symlinks += 1;
                            symlink_entry(&component, mtime_sec(&meta), mtime_nsec(&meta), t)
                        }
                        ferry_platform::LinkDecision::Refuse(reason) => {
                            self.refused.push(RefusedPath {
                                path: child_rel,
                                reason: match reason {
                                    ferry_platform::LinkRefusal::AbsoluteTarget => {
                                        RefusalReason::AbsoluteSymlinkTarget
                                    }
                                    ferry_platform::LinkRefusal::EscapesRoot => {
                                        RefusalReason::EscapingSymlinkTarget
                                    }
                                },
                            });
                            continue;
                        }
                    },
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
                let exec = live_exec(&meta.permissions());
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

        ensure_no_collisions(rel, &entries).map_err(|e| match e {
            ferry_store::snapshot::SnapshotError::NameCollision { parent, name } => {
                ScanError::NameCollision { parent, name }
            }
            ferry_store::snapshot::SnapshotError::CaseCollision {
                parent,
                first,
                second,
            } => ScanError::CaseCollision {
                parent,
                first,
                second,
            },
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
            size: prev_size, ..
        } => {
            prev.mtime_sec == mt.0
                && prev.mtime_nsec == mt.1
                && *prev_size == size
                && prev.exec == exec
        }
        _ => false,
    }
}

/// Exec-bit reading (SPEC subset); needs POSIX mode bits. Hosts without
/// them report false uniformly — the same documented deviation as
/// ferry-materialize's `live_exec` and ferry-store's snapshot walker.
fn live_exec(perm: &std::fs::Permissions) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        const EXEC_MODE_BITS: u32 = 0o111;
        perm.mode() & EXEC_MODE_BITS != 0
    }
    #[cfg(not(unix))]
    {
        let _ = perm;
        false
    }
}

// Metadata accessors mirroring snapshot.rs's rules, now cross-platform:
// mtime via `Metadata::modified()` with timespec-style negatives; hosts that
// cannot represent an mtime read as epoch.
fn split_mtime(t: std::time::SystemTime) -> (i64, u32) {
    ferry_platform::split_unix(t)
}
fn mtime_sec(meta: &std::fs::Metadata) -> i64 {
    meta.modified().map(split_mtime).unwrap_or((0, 0)).0
}
fn mtime_nsec(meta: &std::fs::Metadata) -> u32 {
    meta.modified().map(split_mtime).unwrap_or((0, 0)).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::equivalent_modulo_mtime;
    use crate::testutil::*;
    use ferry_store::crypto::PassthroughCipher;
    use ferry_store::diff::{diff_roots, ChangeSet};
    use ferry_store::snapshot::snapshot_dir;
    use std::collections::BTreeSet;

    /// A seeded scenario: real store + real tree, initial full scan done,
    /// cache seeded — exactly the state an engine hands to incremental
    /// passes. Deterministic tests drive [`Walker::run`] directly, no
    /// threads, no kernel timing.
    struct Fixture {
        _tmp: tempfile::TempDir,
        _store_dir: tempfile::TempDir,
        store: Store,
        poly: u64,
        root: PathBuf,
        cache: DirCache,
        prev_root_tree_id: BlobId,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let store_dir = tempfile::tempdir().unwrap();
            let store =
                Store::create(store_dir.path(), fmk(), Box::new(PassthroughCipher)).unwrap();
            let root = tmp.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            let mut fx = Fixture {
                _tmp: tmp,
                _store_dir: store_dir,
                store,
                poly: poly_of(21),
                root,
                cache: DirCache::new(),
                prev_root_tree_id: [0u8; 32],
            };
            fx.full_scan();
            fx
        }

        /// Full-scan path: what run_full does — whole-tree pass against an
        /// EMPTY cache, then adopt it (the reseed IS the fresh cache).
        fn full_scan(&mut self) -> BlobId {
            let mut fresh = DirCache::new();
            let mut closed = BTreeSet::new();
            closed.insert(Vec::new());
            let mut stats = PassStats::default();
            let out = Walker::run(
                &self.store,
                self.poly,
                &NoIgnoresIgnored,
                &self.root,
                &mut fresh,
                &closed,
                Trigger::Initial,
                &identity((1, 0)),
                self.prev_root_tree_id,
                &mut stats,
            )
            .unwrap();
            std::mem::swap(&mut self.cache, &mut fresh);
            if let Some(out) = out {
                self.prev_root_tree_id = out.root_tree_id;
            }
            self.prev_root_tree_id
        }

        /// Same as [`Self::full_scan`] but returns the refusal ledger.
        fn full_scan_with_ledger(&mut self) -> Vec<ferry_store::snapshot::RefusedPath> {
            let mut fresh = DirCache::new();
            let mut closed = BTreeSet::new();
            closed.insert(Vec::new());
            let mut stats = PassStats::default();
            let out = Walker::run(
                &self.store,
                self.poly,
                &NoIgnoresIgnored,
                &self.root,
                &mut fresh,
                &closed,
                Trigger::Initial,
                &identity((1, 0)),
                self.prev_root_tree_id,
                &mut stats,
            )
            .unwrap();
            let refused = out.as_ref().map(|o| o.refused.clone()).unwrap_or_default();
            std::mem::swap(&mut self.cache, &mut fresh);
            if let Some(out) = out {
                self.prev_root_tree_id = out.root_tree_id;
            }
            refused
        }

        fn incremental(&mut self, dirty: &[RelPath]) -> Option<ScanOutput> {
            let closed = close_under_ancestors(dirty);
            let mut stats = PassStats::default();
            Walker::run(
                &self.store,
                self.poly,
                &NoIgnoresIgnored,
                &self.root,
                &mut self.cache,
                &closed,
                Trigger::Events,
                &identity((2, 0)),
                self.prev_root_tree_id,
                &mut stats,
            )
            .unwrap()
        }

        fn incremental_expect(&mut self, dirty: &[RelPath]) -> ScanOutput {
            self.incremental(dirty)
                .expect("pass must produce a changed manifest")
        }

        /// From-scratch oracle on the CURRENT disk state.
        fn scratch_root_id(&self) -> BlobId {
            snapshot_dir(&self.store, self.poly, &self.root, &identity((3, 0)))
                .unwrap()
                .root_tree_id
        }

        fn diff_since_baseline(&self, new_root: BlobId) -> ChangeSet {
            diff_roots(&self.store, &self.prev_root_tree_id, &new_root).unwrap()
        }
    }

    struct NoIgnoresIgnored;
    impl crate::ignore::IgnorePolicy for NoIgnoresIgnored {
        fn ignored(&self, _rel: &[String]) -> bool {
            false
        }
    }

    fn p(parts: &[&str]) -> RelPath {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mutations_match_from_scratch_oracle_exactly() {
        let mut fx = Fixture::new("tree");
        write_file(&fx.root.join("keep.txt"), b"stable", false, (10, 0));
        write_file(&fx.root.join("gone.txt"), b"to delete", false, (11, 0));
        write_file(&fx.root.join("mod.txt"), b"original", false, (12, 0));
        write_file(&fx.root.join("sub/a.txt"), b"a", false, (13, 0));
        write_file(&fx.root.join("sub/b.txt"), b"b", true, (14, 0));
        write_file(&fx.root.join("moved/x.txt"), b"mover", false, (15, 0));
        fx.full_scan();

        // True mutation set, applied directly to disk:
        //   create nested/new.txt, modify mod.txt (different length),
        //   delete gone.txt, rename dir moved -> relocated (contents ride),
        //   exec-bit flip on sub/a.txt without content change.
        write_file(
            &fx.root.join("nested/new.txt"),
            b"fresh bytes",
            false,
            (21, 1),
        );
        std::fs::remove_file(fx.root.join("gone.txt")).unwrap();
        write_file(
            &fx.root.join("mod.txt"),
            b"original, extended!",
            false,
            (22, 2),
        );
        std::fs::rename(fx.root.join("moved"), fx.root.join("relocated")).unwrap();
        write_file(&fx.root.join("sub/a.txt"), b"a", true, (13, 0));

        // Dirty set = enclosing dirs of every touched path, closed under
        // ancestry — precisely what the engine derives from watcher events.
        let dirty = vec![
            p(&["nested"]),
            p(&["nested", "new.txt"]),
            p(&["gone.txt"]),
            p(&["mod.txt"]),
            p(&["moved"]),
            p(&["relocated"]),
            p(&["sub", "a.txt"]),
        ];
        let out = fx.incremental_expect(&dirty);

        // Strongest form: byte-identical tree to a fresh snapshot_dir.
        assert_eq!(
            out.root_tree_id,
            fx.scratch_root_id(),
            "incremental splice must reproduce a from-scratch snapshot"
        );

        // Diff oracle: incremental change set == scratch change set, i.e.
        // exactly the true mutation set (1 added, 1 removed, 2 content-
        // modified incl. rename-carry, 1 metadata-modified).
        let inc_diff = fx.diff_since_baseline(out.root_tree_id);
        let scratch_diff = fx.diff_since_baseline(fx.scratch_root_id());
        assert_eq!(inc_diff, scratch_diff);
        // Subtree flattening: intermediate dirs appear alongside leaves,
        // parents before children.
        assert_eq!(inc_diff.added.len(), 4);
        let added_paths: Vec<_> = inc_diff.added.iter().map(|a| a.path.clone()).collect();
        assert_eq!(
            added_paths,
            vec![
                p(&["nested"]),
                p(&["nested", "new.txt"]),
                p(&["relocated"]),
                p(&["relocated", "x.txt"])
            ]
        );
        assert_eq!(inc_diff.removed.len(), 3);
        assert_eq!(
            inc_diff
                .removed
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>(),
            vec![p(&["gone.txt"]), p(&["moved"]), p(&["moved", "x.txt"])]
        );
        assert_eq!(inc_diff.content_modified.len(), 1, "{inc_diff:?}");
        assert_eq!(inc_diff.content_modified[0].path, p(&["mod.txt"]));
        assert_eq!(inc_diff.metadata_modified.len(), 1);
        assert_eq!(inc_diff.metadata_modified[0].path, p(&["sub", "a.txt"]));
    }

    #[test]
    fn short_circuit_zero_change_pass_hashes_nothing() {
        let mut fx = Fixture::new("t2");
        write_file(&fx.root.join("x.bin"), &prng(1, 4096), false, (1, 0));
        write_file(&fx.root.join("d/y.bin"), &prng(2, 8192), true, (2, 0));
        fx.full_scan();

        // Mark the WHOLE tree dirty with zero disk changes: everything must
        // hit the size/mtime/exec short-circuit.
        let mut stats = PassStats::default();
        let closed = close_under_ancestors(&[p(&[])]);
        let out = Walker::run(
            &fx.store,
            fx.poly,
            &NoIgnoresIgnored,
            &fx.root,
            &mut fx.cache,
            &closed,
            Trigger::Events,
            &identity((9, 9)),
            fx.prev_root_tree_id,
            &mut stats,
        )
        .unwrap();

        assert!(out.is_none(), "unchanged tree must not emit a manifest");
        assert_eq!(stats.bytes_chunked, 0, "hasher hook proves zero re-hashing");
        assert_eq!(stats.files_rehashed, 0);
        assert!(stats.files > 0, "but files were walked");
    }

    #[test]
    fn audit_catches_same_length_tamper_incremental_misses_it() {
        let mut fx = Fixture::new("t3");
        let victim = fx.root.join("vault.dat");
        let original = prng(7, 1024);
        write_file(&victim, &original, false, (100, 500));
        write_file(&fx.root.join("other.txt"), b" bystander", false, (101, 0));
        fx.full_scan();
        let baseline = fx.prev_root_tree_id;

        // Silent tamper: SAME length, mtime RESTORED — invisible to stat.
        let mut evil = original.clone();
        evil[0] ^= 0xff;
        evil[512] ^= 0x55;
        write_file(&victim, &evil, false, (100, 500));

        // Half 1: an incremental pass over the whole tree may NOT notice...
        let mut stats = PassStats::default();
        let closed = close_under_ancestors(&[p(&[])]);
        let out = Walker::run(
            &fx.store,
            fx.poly,
            &NoIgnoresIgnored,
            &fx.root,
            &mut fx.cache,
            &closed,
            Trigger::Events,
            &identity((4, 0)),
            baseline,
            &mut stats,
        )
        .unwrap();
        assert!(out.is_none());
        assert_eq!(stats.bytes_chunked, 0);
        assert_eq!(fx.diff_since_baseline(baseline), ChangeSet::default());

        // ...but the cache still matches disk-stat, so a subsequent
        // incremental also stays silent. Only a full-hash AUDIT repairs.
        let audited = fx.full_scan();
        assert_ne!(audited, baseline, "audit must catch the drift");
        let cs = diff_roots(&fx.store, &baseline, &audited).unwrap();
        assert_eq!(cs.content_modified.len(), 1);
        assert_eq!(cs.content_modified[0].path, p(&["vault.dat"]));
        assert!(
            !equivalent_modulo_mtime(&fx.store, &baseline, &audited).unwrap(),
            "tampered content must survive normalization (not mtime noise)"
        );
    }

    #[test]
    fn ignored_subtrees_are_pruned_from_walks_and_cache() {
        use crate::ignore::IgnorePolicy;
        struct SkipSecrets;
        impl IgnorePolicy for SkipSecrets {
            fn ignored(&self, rel: &[String]) -> bool {
                rel.first().map(|s| s.as_str()) == Some("secrets")
            }
        }
        let mut fx = Fixture::new("t4");
        write_file(&fx.root.join("open.txt"), b"visible", false, (1, 0));
        write_file(&fx.root.join("secrets/key.pem"), b"hidden", false, (2, 0));
        fx.full_scan();

        // Touch BOTH subtrees; only the open one may enter the manifest.
        write_file(&fx.root.join("open.txt"), b"visible v2", false, (3, 0));
        write_file(&fx.root.join("secrets/key.pem"), b"rotated", false, (4, 0));

        let mut stats = PassStats::default();
        let closed = close_under_ancestors(&[p(&["open.txt"]), p(&["secrets"])]);
        let out = Walker::run(
            &fx.store,
            fx.poly,
            &SkipSecrets,
            &fx.root,
            &mut fx.cache,
            &closed,
            Trigger::Events,
            &identity((5, 0)),
            fx.prev_root_tree_id,
            &mut stats,
        )
        .unwrap()
        .expect("open.txt changed");

        // With ignores active the from-scratch oracle (which knows no ignore
        // rules) is not comparable; assert against the POLICY instead: the
        // ignored subtree never entered the manifest, the visible change did.
        let cs = fx.diff_since_baseline(out.root_tree_id);
        assert_eq!(cs.content_modified.len(), 1);
        assert_eq!(cs.content_modified[0].path, p(&["open.txt"]));
        let root_node = ferry_store::manifest::parse_tree_node(
            &fx.store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap(),
        )
        .unwrap();
        assert!(
            !root_node.entries.iter().any(|e| e.name == "secrets"),
            "ignored dir must be absent from the manifest"
        );
        assert_eq!(stats.bytes_chunked, "visible v2".len() as u64);
    }

    #[test]
    fn deleted_dirty_dir_is_spliced_out_and_cache_pruned() {
        let mut fx = Fixture::new("t5");
        write_file(&fx.root.join("doomed/deep/file.txt"), b"bye", false, (1, 0));
        write_file(&fx.root.join("stays.txt"), b"stay", false, (2, 0));
        fx.full_scan();

        std::fs::remove_dir_all(fx.root.join("doomed")).unwrap();
        let out = fx.incremental_expect(&[p(&["doomed"])]);

        assert_eq!(out.root_tree_id, fx.scratch_root_id());
        assert!(
            fx.cache.node(&p(&["doomed"])).is_none() || {
                // Cache may hold the empty-splice record, but never stale files.
                let node = &fx.cache.node(&p(&["doomed"])).unwrap().node;
                node.entries.is_empty()
            }
        );
        assert!(fx.cache.node(&p(&["doomed", "deep"])).is_none());
        // Diff flattens removed subtrees per path: dir, nested dir, file.
        let cs = fx.diff_since_baseline(out.root_tree_id);
        assert_eq!(cs.removed.len(), 3);
        assert_eq!(
            cs.removed
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>(),
            vec![
                p(&["doomed"]),
                p(&["doomed", "deep"]),
                p(&["doomed", "deep", "file.txt"])
            ],
            "parents before children"
        );
    }

    #[test]
    fn store_directory_is_structurally_excluded() {
        let mut fx = Fixture::new("t7");
        // Store dir appears FIRST, alone: a whole-tree pass must ignore it.
        write_file(
            &fx.root.join(".ferry/packs/x.pack"),
            b"pack bytes",
            false,
            (1, 0),
        );
        let mut stats = PassStats::default();
        let closed = close_under_ancestors(&[p(&[])]);
        let none = Walker::run(
            &fx.store,
            fx.poly,
            &NoIgnoresIgnored,
            &fx.root,
            &mut fx.cache,
            &closed,
            Trigger::Events,
            &identity((1, 5)),
            fx.prev_root_tree_id,
            &mut stats,
        )
        .unwrap();
        assert!(none.is_none(), "store-only content must not move the tree");
        assert_eq!(stats.bytes_chunked, 0);

        // Real user content lands normally.
        write_file(&fx.root.join("real.txt"), b"user content", false, (2, 0));
        let out = fx.incremental_expect(&[p(&["real.txt"])]);
        assert_eq!(out.stats.bytes_chunked, "user content".len() as u64);
        let cs = fx.diff_since_baseline(out.root_tree_id);
        assert_eq!(cs.added.len(), 1, "{cs:?}");
        assert_eq!(cs.added[0].path, p(&["real.txt"]));

        let root_node = ferry_store::manifest::parse_tree_node(
            &fx.store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap(),
        )
        .unwrap();
        assert!(!root_node.entries.iter().any(|e| e.name == ".ferry"));
        assert!(root_node.entries.iter().any(|e| e.name == "real.txt"));

        // Pack churn behind the watcher stays invisible too. The scratch
        // oracle cannot be used alongside .ferry because snapshot_dir has no
        // exclusion rule; the structural exclusion IS the spec here.
        write_file(
            &fx.root.join(".ferry/packs/x.pack"),
            b"more pack bytes!",
            false,
            (3, 0),
        );
        let closed = close_under_ancestors(&[p(&[".ferry"])]);
        let out2 = Walker::run(
            &fx.store,
            fx.poly,
            &NoIgnoresIgnored,
            &fx.root,
            &mut fx.cache,
            &closed,
            Trigger::Events,
            &identity((4, 0)),
            out.root_tree_id,
            &mut stats,
        )
        .unwrap();
        assert!(out2.is_none(), "store-only changes must not move the tree");
        assert_eq!(stats.bytes_chunked, 0);
    }

    #[test]
    fn t12_policy_refusals_match_scratch_oracle_incrementally() {
        // Reserved device names and policy-refused symlinks must behave
        // identically in FULL and INCREMENTAL passes: loud ledger entries,
        // absent from the manifest, with from-scratch parity preserved.
        let mut fx = Fixture::new("t12policy");
        write_file(&fx.root.join("keep.txt"), b"k", false, (1, 0));
        write_file(&fx.root.join("aux.txt"), b"reserved", false, (2, 0));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", fx.root.join("abs_link")).unwrap();
            std::os::unix::fs::symlink("keep.txt", fx.root.join("ok_link")).unwrap();
        }
        let first = fx.full_scan_with_ledger();

        use ferry_store::snapshot::RefusalReason;
        assert!(first.iter().any(|r| r.reason == RefusalReason::ReservedName));
        #[cfg(unix)]
        assert!(first
            .iter()
            .any(|r| r.reason == RefusalReason::AbsoluteSymlinkTarget));

        // Incremental burst over the root keeps tree-id parity with a
        // from-scratch snapshot; refused entries stay out of the manifest.
        write_file(&fx.root.join("keep.txt"), b"k2", false, (3, 0));
        let out = fx.incremental_expect(&[p(&[])]);
        let root = ferry_store::manifest::parse_tree_node(
            &fx.store.get(ferry_store::BlobKind::TreeNode, &out.root_tree_id).unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        let expected: Vec<&str> = if cfg!(unix) {
            vec!["keep.txt", "ok_link"]
        } else {
            vec!["keep.txt"]
        };
        assert_eq!(names, expected);
        assert_eq!(out.root_tree_id, fx.scratch_root_id());
    }

    #[test]
    fn randomized_op_sequence_stays_equivalent_to_scratch() {
        let mut fx = Fixture::new("t6");
        for i in 0..12i64 {
            write_file(
                &fx.root.join(format!("f{i:02}.txt")),
                &prng(i as u64, 64),
                false,
                (i, 0),
            );
        }
        for d in ["d0", "d1"] {
            std::fs::create_dir(fx.root.join(d)).unwrap();
        }
        fx.full_scan();

        // Deterministic pseudo-random op sequence (seeded LCG).
        let mut seed: u64 = 0xC0FFEE;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };

        let mut all_dirty: BTreeSet<RelPath> = BTreeSet::new();
        for step in 0..24usize {
            let pick = next() % 4;
            match pick {
                0 => {
                    let name = format!("f{:02}.txt", next() % 12);
                    write_file(
                        &fx.root.join(&name),
                        &prng(step as u64 + 50, 96),
                        false,
                        (step as i64, 1),
                    );
                    all_dirty.insert(p(&[&name]));
                    all_dirty.insert(p(&[]));
                }
                1 => {
                    let name = format!("gen{step}.txt");
                    write_file(
                        &fx.root.join("d0").join(&name),
                        &prng(100 + step as u64, 32),
                        true,
                        (step as i64, 2),
                    );
                    all_dirty.insert(p(&["d0"]));
                }
                2 => {
                    let name = format!("f{:02}.txt", next() % 12);
                    if std::fs::remove_file(fx.root.join(&name)).is_ok() {
                        all_dirty.insert(p(&[&name]));
                        all_dirty.insert(p(&[]));
                    }
                }
                _ => {
                    let from = format!("f{:02}.txt", next() % 12);
                    let to = format!("ren{step}.txt");
                    if std::fs::rename(fx.root.join(&from), fx.root.join("d1").join(&to)).is_ok() {
                        all_dirty.insert(p(&[&from]));
                        all_dirty.insert(p(&["d1"]));
                        all_dirty.insert(p(&[]));
                    }
                }
            }

            // Batch-settle like the debouncer would: one pass per burst.
            let batch: Vec<RelPath> = std::mem::take(&mut all_dirty).into_iter().collect();
            let closed = close_under_ancestors(&batch);
            let mut stats = PassStats::default();
            let out = Walker::run(
                &fx.store,
                fx.poly,
                &NoIgnoresIgnored,
                &fx.root,
                &mut fx.cache,
                &closed,
                Trigger::Events,
                &identity((6, 0)),
                fx.prev_root_tree_id,
                &mut stats,
            )
            .unwrap();
            if let Some(out) = out {
                fx.prev_root_tree_id = out.root_tree_id;
            }
            assert_eq!(
                fx.prev_root_tree_id,
                fx.scratch_root_id(),
                "after op {step}, live tree must equal a from-scratch snapshot"
            );
        }
    }
}
