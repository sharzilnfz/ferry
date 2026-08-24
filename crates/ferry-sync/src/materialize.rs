//! Materializer seam + M0's throwaway inline applier.
//!
//! The trait is the deliverable; T-005's real applier implements it against
//! the same inputs (target manifest, change set, blob source). This inline
//! version is ugly-but-correct:
//!
//! - files: write to a temp file in the destination directory, set mode and
//!   mtime on the temp file, then atomic rename over the final name;
//! - creations parents-first (create_dir_all), deletions children-first
//!   (reverse path order — `diff` sorts ascending so reversing walks leaves
//!   before roots);
//! - type changes delete-then-create;
//! - directory mtimes are stamped LAST, deepest first, from the TARGET tree
//!   itself. This is not cosmetic: manifests carry dir mtimes, and if the
//!   materialized tree did not reproduce them exactly, each side's next
//!   snapshot would produce a different root id and sync would never settle.
//!   The same logic stamps ancestors of modified files that appear nowhere
//!   in the change set but whose mtimes moved in the donor's snapshot;
//! - exec bit only (SPEC permission subset); other mode bits are dropped by
//!   design;
//! - symlink targets are content; link mtimes are restored via
//!   `utimensat(AT_SYMLINK_NOFOLLOW)` because std cannot open a link itself
//!   without following it. Since T-012 the implementation lives in
//!   ferry-materialize (`set_symlink_times`) and is shared from there;
//!   non-unix builds refuse symlinks loudly instead of silently
//!   mis-syncing them.
//!
//! Empty files have zero chunks and materialize as empty files; nothing is
//! fetched from the store for them (the store refuses empty blobs).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ferry_store::diff::{ChangeSet, CompPath, EntryKind, EntryState};
use ferry_store::manifest::{parse_tree_node, EntryPayload, RootManifest};
use ferry_store::{BlobId, BlobKind};

#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    #[error("io failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("blob unavailable: {0}")]
    MissingBlob(String),
    #[error("stored tree node corrupt: {0}")]
    Tree(#[from] ferry_store::manifest::ManifestError),
    #[error("{0}")]
    Unsupported(&'static str),
}

fn io(path: &Path, source: std::io::Error) -> MaterializeError {
    MaterializeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Read-only view over whatever holds blobs locally. The engine plugs the
/// store in; tests plug fakes in; T-005 can do the same.
pub trait BlobSource {
    fn get(&self, kind: BlobKind, id: &BlobId) -> Result<Vec<u8>, MaterializeError>;
}

/// Applies a change set to bring a tree to the target manifest's state.
pub trait Materializer {
    fn apply(
        &mut self,
        target: &RootManifest,
        changes: &ChangeSet,
        blobs: &dyn BlobSource,
    ) -> Result<(), MaterializeError>;
}

/// M0's inline implementation, rooted at one physical directory.
pub struct InlineMaterializer {
    root: PathBuf,
}

impl InlineMaterializer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        InlineMaterializer { root: root.into() }
    }

    fn resolve(&self, rel: &[String]) -> PathBuf {
        let mut p = self.root.clone();
        for c in rel {
            p.push(c);
        }
        p
    }

    fn ensure_parent(&self, path: &Path) -> Result<(), MaterializeError> {
        let parent = path
            .parent()
            .ok_or(MaterializeError::Unsupported("path without parent"))?;
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))
    }

    fn remove_any(&self, path: &Path, dir_hint: bool) -> Result<(), MaterializeError> {
        let meta = match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io(path, source)),
            Ok(meta) => meta,
        };
        let res = if meta.is_dir() || (dir_hint && !meta.is_symlink()) {
            // remove_dir first keeps the children-first contract honest;
            // remove_dir_all is the defensive fallback for stragglers.
            std::fs::remove_dir(path).or_else(|_| std::fs::remove_dir_all(path))
        } else {
            std::fs::remove_file(path)
        };
        res.map_err(|e| io(path, e))
    }
}

impl Materializer for InlineMaterializer {
    fn apply(
        &mut self,
        target: &RootManifest,
        changes: &ChangeSet,
        blobs: &dyn BlobSource,
    ) -> Result<(), MaterializeError> {
        // ---- phase 1: deletions, children first -------------------------
        // diff flattens removed subtrees parents-before-children and sorts
        // ascending, so reverse order deletes leaves before their parents.
        // Type changes also retire their OLD incarnation here.
        let mut removals: Vec<(CompPath, bool)> = changes
            .removed
            .iter()
            .map(|r| (r.path.clone(), r.state.kind == EntryKind::Dir))
            .chain(
                changes
                    .type_changed
                    .iter()
                    .map(|m| (m.path.clone(), m.before.kind == EntryKind::Dir)),
            )
            .collect();
        removals.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, dir_hint) in removals.iter().rev() {
            self.remove_any(&self.resolve(path), *dir_hint)?;
        }

        // ---- phase 2: upserts -------------------------------------------
        let mut upserts: Vec<(&CompPath, &EntryState)> = Vec::new();
        for a in &changes.added {
            upserts.push((&a.path, &a.state));
        }
        for m in &changes.content_modified {
            upserts.push((&m.path, &m.after));
        }
        for m in &changes.metadata_modified {
            upserts.push((&m.path, &m.after));
        }
        for m in &changes.type_changed {
            upserts.push((&m.path, &m.after));
        }

        for (i, (path, state)) in upserts.into_iter().enumerate() {
            let final_path = self.resolve(path);
            self.ensure_parent(&final_path)?;
            match state.kind {
                EntryKind::Dir => {
                    // A non-dir squatting on the name must go; an existing
                    // dir stays (its children are handled separately).
                    if let Ok(meta) = std::fs::symlink_metadata(&final_path) {
                        if !meta.is_dir() {
                            self.remove_any(&final_path, false)?;
                        }
                    }
                    std::fs::create_dir_all(&final_path).map_err(|e| io(&final_path, e))?;
                }
                EntryKind::File => {
                    write_file_from_chunks(
                        &final_path,
                        &state.chunks,
                        state.exec,
                        (state.mtime_sec, state.mtime_nsec),
                        blobs,
                        i,
                    )?;
                }
                EntryKind::Symlink => {
                    let t = state
                        .target
                        .as_deref()
                        .ok_or(MaterializeError::Unsupported(
                            "symlink state missing target",
                        ))?;
                    upsert_symlink(&final_path, t, (state.mtime_sec, state.mtime_nsec))?;
                }
            }
        }

        // ---- phase 3: stamp every directory mtime from the target tree --
        // Deepest first so later parent stamps never clobber child stamps
        // on filesystems where touching a parent perturbs nothing else.
        let mut dirs: Vec<(CompPath, i64, u32)> = Vec::new();
        collect_dirs(blobs, &target.root_tree_id, Vec::new(), &mut dirs)?;
        dirs.sort_by_key(|d| std::cmp::Reverse(d.0.len()));
        for (rel, sec, nsec) in dirs {
            set_regular_times(&self.resolve(&rel), sec, nsec)?;
        }
        Ok(())
    }
}

fn write_file_from_chunks(
    final_path: &Path,
    chunks: &[(BlobId, u64)],
    exec: bool,
    mtime: (i64, u32),
    blobs: &dyn BlobSource,
    seq: usize,
) -> Result<(), MaterializeError> {
    let dir = final_path
        .parent()
        .ok_or(MaterializeError::Unsupported("no parent"))?;
    // Same-directory temp name, unique per process + call site.
    let tmp = dir.join(format!(
        ".ferry-m0-tmp-{}-{seq}-{}",
        std::process::id(),
        &blake3::hash(final_path.to_string_lossy().as_bytes()).to_hex()[..12]
    ));

    let write_res = (|| {
        let mut out = std::fs::File::create(&tmp).map_err(|e| io(&tmp, e))?;
        for (id, len) in chunks {
            let bytes = blobs.get(BlobKind::DataChunk, id)?;
            if bytes.len() as u64 != *len {
                return Err(MaterializeError::MissingBlob(format!(
                    "chunk {} declared {len} bytes but produced {}",
                    ferry_store::format::hex(id),
                    bytes.len()
                )));
            }
            out.write_all(&bytes).map_err(|e| io(&tmp, e))?;
        }
        apply_exec(&tmp, exec)?;
        out.set_times(std::fs::FileTimes::new().set_modified(system_time(mtime)?))
            .map_err(|e| io(&tmp, e))?;
        out.sync_all().map_err(|e| io(&tmp, e))?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // rename(2) cannot replace a directory: clear the way if one squats on
    // the final name (type flips handle most cases; this is the remainder).
    if let Ok(meta) = std::fs::symlink_metadata(final_path) {
        if meta.is_dir() && !meta.is_symlink() {
            std::fs::remove_dir_all(final_path).map_err(|e| io(final_path, e))?;
        }
    }
    std::fs::rename(&tmp, final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(final_path, e)
    })
}

#[cfg(unix)]
fn apply_exec(path: &Path, exec: bool) -> Result<(), MaterializeError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .map_err(|e| io(path, e))?
        .permissions();
    let mode = if exec { 0o755 } else { 0o644 };
    if perm.mode() & 0o777 != mode {
        perm.set_mode(mode);
        std::fs::set_permissions(path, perm).map_err(|e| io(path, e))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_exec(_path: &Path, _exec: bool) -> Result<(), MaterializeError> {
    // The permission subset beyond read/write does not exist everywhere;
    // T-012 owns the cross-platform story.
    Ok(())
}

/// i64 seconds + u32 nanoseconds -> SystemTime (pre-epoch safe).
fn system_time((sec, nsec): (i64, u32)) -> Result<std::time::SystemTime, MaterializeError> {
    use std::time::{Duration, UNIX_EPOCH};
    if nsec > 999_999_999 {
        return Err(MaterializeError::Unsupported("mtime nsec out of range"));
    }
    if sec >= 0 {
        Ok(UNIX_EPOCH + Duration::new(sec as u64, nsec))
    } else {
        Ok(UNIX_EPOCH
            - Duration::new((-sec) as u64, 0)
            - Duration::from_nanos(1_000_000_000u64.saturating_sub(nsec as u64)))
    }
}

#[cfg(unix)]
fn upsert_symlink(path: &Path, target: &str, mtime: (i64, u32)) -> Result<(), MaterializeError> {
    use std::os::unix::fs::symlink;

    // Recreate only when needed: an identical existing link keeps working
    // and only needs its times refreshed.
    let needs_create = match std::fs::read_link(path) {
        Ok(existing) => existing.to_string_lossy() != target,
        Err(_) => true, // missing or occupied by a non-link
    };
    if needs_create {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            let res = if meta.is_dir() && !meta.is_symlink() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            res.map_err(|e| io(path, e))?;
        }
        symlink(target, path).map_err(|e| io(path, e))?;
    }
    set_symlink_times(path, mtime)
}

#[cfg(not(unix))]
fn upsert_symlink(_path: &Path, _target: &str, _mtime: (i64, u32)) -> Result<(), MaterializeError> {
    Err(MaterializeError::Unsupported(
        "symlinks unsupported on this platform",
    ))
}

/// std cannot touch a symlink's own times (any open follows the link).
/// Implementation shared with ferry-materialize since T-012.
#[cfg(unix)]
fn set_symlink_times(path: &Path, (sec, nsec): (i64, u32)) -> Result<(), MaterializeError> {
    ferry_materialize::set_symlink_times(path, sec, nsec).map_err(|e| match e {
        ferry_materialize::MaterializeError::Io { path, source } => {
            MaterializeError::Io { path, source }
        }
        _ => MaterializeError::Unsupported("symlink mtime restoration failed"),
    })
}

fn set_regular_times(path: &Path, sec: i64, nsec: u32) -> Result<(), MaterializeError> {
    #[cfg(unix)]
    {
        // Read-only handle suffices: futimens checks OWNERSHIP, not write
        // access — so this works on directories too.
        let f = std::fs::File::open(path).map_err(|e| io(path, e))?;
        f.set_times(std::fs::FileTimes::new().set_modified(system_time((sec, nsec))?))
            .map_err(|e| io(path, e))
    }
    #[cfg(not(unix))]
    {
        // std cannot open directory handles on windows; phase 3 stamps
        // DIRECTORY mtimes through here, so use SetFileTime via filetime,
        // which handles files and directories alike.
        let ft = filetime::FileTime::from_unix_time(sec, nsec);
        filetime::set_file_mtime(path, ft).map_err(|e| io(path, e))
    }
}

/// Walk the target tree collecting `(path, mtime)` for every DIRECTORY so
/// phase 3 can stamp them. Loads tree nodes through the blob source.
fn collect_dirs(
    blobs: &dyn BlobSource,
    tree_id: &BlobId,
    prefix: CompPath,
    out: &mut Vec<(CompPath, i64, u32)>,
) -> Result<(), MaterializeError> {
    let bytes = blobs.get(BlobKind::TreeNode, tree_id)?;
    let node = parse_tree_node(&bytes)?;
    for e in node.entries {
        let mut path = prefix.clone();
        path.push(e.name);
        if let EntryPayload::Dir { child_tree_id } = e.payload {
            out.push((path.clone(), e.mtime_sec, e.mtime_nsec));
            collect_dirs(blobs, &child_tree_id, path, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::diff::{Added, Modified, Removed};
    use ferry_store::manifest::{dir_entry, file_entry, TreeNode};
    use std::collections::HashMap;

    /// In-memory blob source over pre-loaded chunks/tree nodes.
    struct FakeBlobs {
        chunks: HashMap<BlobId, Vec<u8>>,
        trees: HashMap<BlobId, Vec<u8>>,
    }

    impl FakeBlobs {
        fn new(store: &ferry_store::store::Store, bytes: &[u8]) -> (Self, BlobId, u64) {
            let id = store.put_data(bytes).unwrap();
            (
                FakeBlobs {
                    chunks: HashMap::from([(id, bytes.to_vec())]),
                    trees: HashMap::new(),
                },
                id,
                bytes.len() as u64,
            )
        }

        /// Trees keyed by all-zero ids resolve to the canonical empty node,
        /// so changeset-only tests can pass a placeholder root manifest.
        fn with_empty_tree() -> Self {
            let empty = ferry_store::manifest::serialize_tree_node(&TreeNode {
                entries: Vec::new(),
            });
            FakeBlobs {
                chunks: HashMap::new(),
                trees: HashMap::from([([0u8; 32], empty)]),
            }
        }
    }

    impl BlobSource for FakeBlobs {
        fn get(&self, kind: BlobKind, id: &BlobId) -> Result<Vec<u8>, MaterializeError> {
            let store = match kind {
                BlobKind::DataChunk => &self.chunks,
                BlobKind::TreeNode => &self.trees,
                _ => return Err(MaterializeError::MissingBlob("kind unused".into())),
            };
            store
                .get(id)
                .cloned()
                .ok_or_else(|| MaterializeError::MissingBlob(ferry_store::format::hex(id)))
        }
    }

    fn entry_state(kind: EntryKind, exec: bool, mtime: (i64, u32)) -> EntryState {
        EntryState {
            kind,
            exec,
            mtime_sec: mtime.0,
            mtime_nsec: mtime.1,
            chunks: Vec::new(),
            target: None,
        }
    }

    fn empty_manifest(root: BlobId) -> RootManifest {
        RootManifest {
            folder_id: [0; 16],
            device_id: [0; 32],
            created_sec: 0,
            created_nsec: 0,
            root_tree_id: root,
            parent_manifest_id: [0; 32],
        }
    }

    #[test]
    fn creates_nested_files_parents_first_with_exact_bytes_mode_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = tempfile::tempdir().unwrap();
        let store = ferry_store::store::Store::create(
            sdir.path(),
            [0u8; 32],
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap();

        let (fb0, c1, l1) = FakeBlobs::new(&store, b"deep payload");
        // Second chunk into the same fake source; keep the empty-tree stub
        // so phase 3 can resolve the placeholder root id.
        let extra = b"!";
        let c2 = store.put_data(extra).unwrap();
        let mut fb = fb0;
        fb.chunks.insert(c2, extra.to_vec());
        fb.trees.insert(
            [0u8; 32],
            ferry_store::manifest::serialize_tree_node(&TreeNode {
                entries: Vec::new(),
            }),
        );

        let mut st = entry_state(EntryKind::File, true, (1_700_000_123, 456));
        st.chunks = vec![(c1, l1), (c2, extra.len() as u64)];

        let changes = ChangeSet {
            added: vec![Added {
                path: vec!["a".into(), "b".into(), "c.txt".into()],
                state: st,
            }],
            ..Default::default()
        };
        InlineMaterializer::new(dir.path())
            .apply(&empty_manifest([0; 32]), &changes, &fb)
            .unwrap();

        let got = std::fs::read(dir.path().join("a/b/c.txt")).unwrap();
        assert_eq!(got, b"deep payload!");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("a/b/c.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "exec bit applied");
        }
        let m = std::fs::metadata(dir.path().join("a/b/c.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let want_ns: u128 = 1_700_000_123u128 * 1_000_000_000 + 456;
        assert_eq!(
            m.duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            want_ns,
            "mtime restored exactly"
        );
    }

    #[test]
    fn empty_file_materializes_as_empty_without_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FakeBlobs::with_empty_tree();
        let changes = ChangeSet {
            added: vec![Added {
                path: vec!["void".into()],
                state: entry_state(EntryKind::File, false, (5, 0)),
            }],
            ..Default::default()
        };
        InlineMaterializer::new(dir.path())
            .apply(&empty_manifest([0; 32]), &changes, &fb)
            .unwrap();
        assert_eq!(std::fs::metadata(dir.path().join("void")).unwrap().len(), 0);
    }

    #[test]
    fn deletions_run_children_before_parents_and_type_changes_swap_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let fb = FakeBlobs::with_empty_tree();

        std::fs::create_dir_all(dir.path().join("gone/sub")).unwrap();
        std::fs::write(dir.path().join("gone/sub/f.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("flip"), b"file").unwrap();

        let rm_child = Removed {
            path: vec!["gone".into(), "sub".into()],
            state: entry_state(EntryKind::Dir, false, (0, 0)),
        };
        let rm_parent = Removed {
            path: vec!["gone".into()],
            state: entry_state(EntryKind::Dir, false, (0, 0)),
        };
        let flip = Modified {
            path: vec!["flip".into()],
            before: entry_state(EntryKind::File, false, (0, 0)),
            after: entry_state(EntryKind::Dir, false, (0, 0)),
        };
        let changes = ChangeSet {
            removed: vec![rm_parent, rm_child],
            type_changed: vec![flip],
            ..Default::default()
        };
        InlineMaterializer::new(dir.path())
            .apply(&empty_manifest([0; 32]), &changes, &fb)
            .unwrap();

        assert!(
            !dir.path().join("gone").exists(),
            "subtree deleted leaves-first"
        );
        assert!(dir.path().join("flip").is_dir(), "file became dir");
    }

    #[test]
    fn ancestor_directory_mtimes_come_from_the_target_tree() {
        let sdir = tempfile::tempdir().unwrap();
        let store = ferry_store::store::Store::create(
            sdir.path(),
            [0u8; 32],
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap();

        // Target: root -> inner(dir, mtime 111.222) -> deep.txt
        let leaf_id = store
            .put_meta(
                BlobKind::TreeNode,
                &serialize_tree(&TreeNode {
                    entries: vec![file_entry("deep.txt", false, 7, 7, vec![])],
                }),
            )
            .unwrap();
        let inner_node = TreeNode {
            entries: vec![dir_entry("inner", 111, 222, leaf_id)],
        };
        let inner_id = store
            .put_meta(BlobKind::TreeNode, &serialize_tree(&inner_node))
            .unwrap();

        let mut fb = FakeBlobs::with_empty_tree();
        fb.trees
            .insert(inner_id, store.get(BlobKind::TreeNode, &inner_id).unwrap());
        fb.trees
            .insert(leaf_id, store.get(BlobKind::TreeNode, &leaf_id).unwrap());

        // The change set adds ONLY the deep file — the ancestor dir appears
        // nowhere, yet its mtime must still be stamped from the target.
        let changes = ChangeSet {
            added: vec![Added {
                path: vec!["inner".into(), "deep.txt".into()],
                state: entry_state(EntryKind::File, false, (7, 7)),
            }],
            ..Default::default()
        };

        let dir = tempfile::tempdir().unwrap();
        let mut target = empty_manifest(inner_id);
        target.root_tree_id = inner_id;
        InlineMaterializer::new(dir.path())
            .apply(&target, &changes, &fb)
            .unwrap();

        let m = std::fs::metadata(dir.path().join("inner"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            m.duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64,
            111 * 1_000_000_000 + 222,
            "ancestor dir mtime comes from the offered tree"
        );
        assert_eq!(
            std::fs::read(dir.path().join("inner/deep.txt")).unwrap(),
            b"",
            "zero-chunk file is an empty file"
        );
    }

    #[test]
    fn symlinks_get_exact_target_and_own_mtime() {
        #[cfg(not(unix))]
        {
            // Non-unix: materializing a symlink fails loudly, never silently.
            let dir = tempfile::tempdir().unwrap();
            let fb = FakeBlobs::with_empty_tree();
            let mut st = entry_state(EntryKind::Symlink, false, (0, 0));
            st.target = Some("elsewhere".into());
            let changes = ChangeSet {
                added: vec![Added {
                    path: vec!["lnk".into()],
                    state: st,
                }],
                ..Default::default()
            };
            assert!(InlineMaterializer::new(dir.path())
                .apply(&empty_manifest([0; 32]), &changes, &fb)
                .is_err());
        }

        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let fb = FakeBlobs::with_empty_tree();
            let mut st = entry_state(EntryKind::Symlink, false, (1_700_000_000, 42));
            st.target = Some("../shared/secret.txt".into());
            let changes = ChangeSet {
                added: vec![Added {
                    path: vec!["sub".into(), "lnk".into()],
                    state: st,
                }],
                ..Default::default()
            };
            InlineMaterializer::new(dir.path())
                .apply(&empty_manifest([0; 32]), &changes, &fb)
                .unwrap();

            let p = dir.path().join("sub/lnk");
            assert_eq!(
                std::fs::read_link(&p).unwrap().to_string_lossy(),
                "../shared/secret.txt"
            );
            let ns = std::fs::symlink_metadata(&p)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let want_ns: u128 = 1_700_000_000u128 * 1_000_000_000 + 42;
            assert_eq!(ns, want_ns, "link's OWN mtime restored");
        }
    }

    fn serialize_tree(n: &TreeNode) -> Vec<u8> {
        ferry_store::manifest::serialize_tree_node(n)
    }
}
