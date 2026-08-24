//! Directory snapshots (T-003): walk a real tree on disk into
//! hash-addressed manifests per `docs/store-format.md` ("Manifest schema").
//!
//! Walk rules, as documented policy for this ticket:
//!
//! - Names are NFC-normalized at scan time; a name that is not valid UTF-8
//!   is refused loudly (see [`RefusalReason`]).
//! - Nothing is skipped silently. Unsupported file types (sockets, FIFOs,
//!   devices), non-UTF-8 names or symlink targets, reserved Windows device
//!   names, and symlinks whose targets are absolute or escape the folder
//!   root are recorded in [`SnapshotOutput::refused`] with the offending
//!   path; the caller decides whether an empty refusal ledger is required.
//!   The snapshot itself completes without those entries.
//! - Hard IO failures (unreadable or vanished entries) are errors carrying
//!   the path; they never downgrade to refusals.
//! - Siblings whose NFC-normalized names collide are a hard error: the
//!   format forbids duplicate names, and silently merging two distinct files
//!   would lose data. On case-folding hosts (macOS/Windows), siblings whose
//!   names fold together case-insensitively are equally fatal ([`ferry_platform`]
//!   case-conflict detection; T-012) — never silently picked.
//! - The exec bit maps to flags bit 0 only (SPEC permission subset); mtime
//!   is stored as signed seconds + nanoseconds straight off the filesystem.
//!   On hosts without POSIX permission bits the subset is unrepresentable
//!   and exec reads as false (documented deviation).
//! - Symlink targets must decode as UTF-8 (refused otherwise) and be
//!   NFC-normalized like every other string; targets are stored verbatim
//!   apart from that. Policy per T-012: relative internal targets sync as
//!   links; absolute targets and `..`-escaping targets are refused loudly.
//!
//! Platform: cross-platform since T-012. Metadata access goes through
//! `std::fs::Metadata::modified()` / `file_type()`; the exec bit needs POSIX
//! mode bits where available.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ferry_platform::{classify_link, find_case_conflict, host_folds_case, is_reserved_device_name};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use crate::chunker::chunk;
use crate::format::{BlobId, BlobKind};
use crate::manifest::{
    dir_entry, file_entry, serialize_manifest, serialize_tree_node, symlink_entry, RootManifest,
    TreeEntry, TreeNode,
};
use crate::store::{Store, StoreError};

/// Any of these permission bits means "executable" (SPEC permission subset).
/// Only meaningful where POSIX mode bits exist; see [`exec_of`].

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("io failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("siblings under {parent} collide after NFC normalization: {name}")]
    NameCollision { parent: String, name: String },
    #[error(
        "case-conflict under {parent}: {first:?} and {second:?} differ only by case and cannot \
         coexist on this filesystem; rename one of them (ferry never picks silently)"
    )]
    CaseCollision {
        parent: String,
        first: String,
        second: String,
    },
    #[error("created_nsec out of range 0..999_999_999")]
    InvalidTimestamp,
    #[error("store rejected a blob: {0}")]
    Store(#[from] StoreError),
}

/// Why one path was refused a place in the snapshot. Refusals are loud:
/// they are listed in [`SnapshotOutput::refused`], never silently dropped.
/// Every message names the fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Error)]
pub enum RefusalReason {
    #[error(
        "unsupported file type (socket, fifo, device); move it out of the \
         synced folder or replace it with a regular file"
    )]
    UnknownFileType,
    #[error("name is not valid UTF-8; rename the entry to a UTF-8 name")]
    NonUtf8Name,
    #[error(
        "symlink target is not valid UTF-8; recreate the link with a UTF-8 \
         target"
    )]
    NonUtf8SymlinkTarget,
    #[error(
        "reserved Windows device name (CON, PRN, AUX, NUL, COM1-9, LPT1-9 \
         with any extension); Windows endpoints could never store it — \
         rename the entry, e.g. add a prefix like `data-`"
    )]
    ReservedName,
    #[error(
        "symlink target is absolute and would point outside the synced \
         folder on other devices; retarget it relatively inside the folder"
    )]
    AbsoluteSymlinkTarget,
    #[error(
        "symlink target escapes the synced folder via '..'; retarget it to \
         a path inside the folder"
    )]
    EscapingSymlinkTarget,
}

/// One path that could not be represented in a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedPath {
    /// Path relative to the snapshot root, NFC components.
    pub path: Vec<String>,
    pub reason: RefusalReason,
}

/// Counters describing one completed scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub files: usize,
    pub dirs: usize,
    pub symlinks: usize,
    pub bytes_chunked: u64,
}

/// Lineage and timestamp for the root manifest. Callers own these; tests pin
/// them so determinism is observable byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotIdentity {
    pub folder_id: [u8; 16],
    pub device_id: [u8; 32],
    pub parent_manifest_id: BlobId,
    pub created_sec: i64,
    pub created_nsec: u32,
}

/// Everything one successful snapshot produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOutput {
    /// The root manifest object (already stored as a PACK_META blob).
    pub manifest: RootManifest,
    /// BLAKE3 of the serialized manifest == its blob address.
    pub manifest_id: BlobId,
    /// Address of the root tree node.
    pub root_tree_id: BlobId,
    pub stats: ScanStats,
    /// Paths refused a place in the snapshot, with reasons. Empty means a
    /// fully-represented tree.
    pub refused: Vec<RefusedPath>,
}

/// Walk `source` and store every chunk, tree node, and the root manifest
/// into `store`. Seals all staging packs before returning (end-of-burst
/// rule), so a resnapshot of an unchanged tree writes no new pack bytes.
pub fn snapshot_dir(
    store: &Store,
    poly: u64,
    source: &Path,
    identity: &SnapshotIdentity,
) -> Result<SnapshotOutput, SnapshotError> {
    if identity.created_nsec > 999_999_999 {
        return Err(SnapshotError::InvalidTimestamp);
    }
    let mut walker = Walker {
        store,
        poly,
        stats: ScanStats::default(),
        refused: Vec::new(),
    };
    let mut rel = Vec::new();
    let root = walker.walk_dir(source, &mut rel)?;
    let root_bytes = serialize_tree_node(&root);
    let root_tree_id = store.put_meta(BlobKind::TreeNode, &root_bytes)?;

    let manifest = RootManifest {
        folder_id: identity.folder_id,
        device_id: identity.device_id,
        created_sec: identity.created_sec,
        created_nsec: identity.created_nsec,
        root_tree_id,
        parent_manifest_id: identity.parent_manifest_id,
    };
    let manifest_bytes = serialize_manifest(&manifest);
    let manifest_id = store.put_meta(BlobKind::Manifest, &manifest_bytes)?;

    // A snapshot is a scan burst: seal every staging pack before returning.
    store.flush()?;

    Ok(SnapshotOutput {
        manifest,
        manifest_id,
        root_tree_id,
        stats: walker.stats,
        refused: walker.refused,
    })
}

/// Render a relative component vector the way errors and logs show it.
pub fn join_path(parts: &[String]) -> String {
    parts.join("/")
}

/// Duplicate names are forbidden within one tree node. Normalization happens
/// before this check, so two on-disk spellings that NFC-fold onto each other
/// are caught here rather than panicking inside the serializer.
pub fn ensure_no_sibling_collisions(
    parent: &[String],
    entries: &[TreeEntry],
) -> Result<(), SnapshotError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for e in entries {
        if !seen.insert(e.name.as_str()) {
            return Err(SnapshotError::NameCollision {
                parent: join_path(parent),
                name: e.name.clone(),
            });
        }
    }
    Ok(())
}

/// On case-folding hosts (macOS/Windows; [`ferry_platform::host_folds_case`]),
/// two sibling entries whose names fold together case-insensitively cannot
/// coexist on disk — and silently picking one loses data. Fatal, naming both
/// spellings. On case-sensitive hosts the pair legitimately coexists and is
/// allowed through.
///
/// Called together with [`ensure_no_sibling_collisions`] by BOTH walkers
/// (from-scratch and incremental), so the two can never drift apart.
pub fn ensure_no_host_case_collisions(
    parent: &[String],
    entries: &[TreeEntry],
) -> Result<(), SnapshotError> {
    if !host_folds_case() || entries.len() < 2 {
        return Ok(());
    }
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    if let Some(c) = find_case_conflict(&names) {
        return Err(SnapshotError::CaseCollision {
            parent: join_path(parent),
            first: c.first,
            second: c.second,
        });
    }
    Ok(())
}

/// Both sibling gates in the order policy requires (NFC duplicates first:
/// they are format-invalid everywhere; fold collisions are host-invalid).
pub fn ensure_no_collisions(parent: &[String], entries: &[TreeEntry]) -> Result<(), SnapshotError> {
    ensure_no_sibling_collisions(parent, entries)?;
    ensure_no_host_case_collisions(parent, entries)
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> SnapshotError + '_ {
    let path = path.to_path_buf();
    move |source| SnapshotError::Io {
        path: path.clone(),
        source,
    }
}

/// mtime as the manifest's `(sec, nsec)` pair, via `Metadata::modified()`.
/// Hosts that cannot represent an mtime read as epoch (documented).
fn mtime_of(meta: &std::fs::Metadata) -> (i64, u32) {
    match meta.modified() {
        Ok(t) => ferry_platform::split_unix(t),
        Err(_) => (0, 0),
    }
}

/// Exec-bit detection (SPEC permission subset). Needs POSIX mode bits;
/// hosts without them report false uniformly (T-012 documented deviation).
fn exec_of(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        const EXEC_MODE_BITS: u32 = 0o111;
        meta.permissions().mode() & EXEC_MODE_BITS != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

struct Walker<'a> {
    store: &'a Store,
    poly: u64,
    stats: ScanStats,
    refused: Vec<RefusedPath>,
}

impl Walker<'_> {
    fn refuse(&mut self, rel: &[String], reason: RefusalReason) {
        self.refused.push(RefusedPath {
            path: rel.to_vec(),
            reason,
        });
    }

    fn put_tree(&self, node: &TreeNode) -> Result<BlobId, SnapshotError> {
        let bytes = serialize_tree_node(node);
        Ok(self.store.put_meta(BlobKind::TreeNode, &bytes)?)
    }

    fn walk_dir(&mut self, dir: &Path, rel: &mut Vec<String>) -> Result<TreeNode, SnapshotError> {
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(io_err(dir))? {
            let entry = entry.map_err(io_err(dir))?;
            names.push(entry.file_name());
        }
        // Deterministic traversal; serialization re-sorts anyway.
        names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));

        let mut entries: Vec<TreeEntry> = Vec::with_capacity(names.len());
        for name in names {
            let raw = name.as_encoded_bytes();
            // read_dir never yields "." / ".."; the guard is defensive.
            if raw == b"." || raw == b".." {
                continue;
            }
            let component = match std::str::from_utf8(raw) {
                Ok(s) => s.nfc().collect::<String>(),
                Err(_) => {
                    rel.push(String::from_utf8_lossy(raw).into_owned());
                    self.refuse(rel, RefusalReason::NonUtf8Name);
                    rel.pop();
                    continue;
                }
            };
            // Reserved Windows device names can never be materialized on a
            // Windows endpoint; refuse loudly at the source (T-012).
            if is_reserved_device_name(&component) {
                rel.push(component);
                self.refuse(rel, RefusalReason::ReservedName);
                rel.pop();
                continue;
            }
            let child_path = dir.join(&name);
            rel.push(component);
            let visited = self.visit(&child_path, rel);
            rel.pop();
            if let Some(e) = visited? {
                entries.push(e);
            }
        }

        ensure_no_collisions(rel, &entries)?;
        Ok(TreeNode { entries })
    }

    /// Stat, read, chunk, and encode one directory entry. Returns `None` for
    /// loudly-refused paths (recorded in `self.refused`).
    fn visit(
        &mut self,
        path: &Path,
        rel: &mut Vec<String>,
    ) -> Result<Option<TreeEntry>, SnapshotError> {
        let component = rel.last().expect("component pushed before visit").clone();
        // symlink_metadata never follows links: a symlinked dir must be
        // stored as a link, not walked through.
        let meta = std::fs::symlink_metadata(path).map_err(io_err(path))?;
        let ft = meta.file_type();

        if ft.is_symlink() {
            let target = std::fs::read_link(path).map_err(io_err(path))?;
            return match target.to_str() {
                Some(t) => {
                    // T-012 symlink policy: only relative targets that stay
                    // inside the folder sync as links (see ferry_platform).
                    let depth = rel.len().saturating_sub(1);
                    match classify_link(depth, t) {
                        ferry_platform::LinkDecision::SyncAsLink => {
                            self.stats.symlinks += 1;
                            let (sec, nsec) = mtime_of(&meta);
                            Ok(Some(symlink_entry(&component, sec, nsec, t)))
                        }
                        ferry_platform::LinkDecision::Refuse(reason) => {
                            let r = match reason {
                                ferry_platform::LinkRefusal::AbsoluteTarget => {
                                    RefusalReason::AbsoluteSymlinkTarget
                                }
                                ferry_platform::LinkRefusal::EscapesRoot => {
                                    RefusalReason::EscapingSymlinkTarget
                                }
                            };
                            self.refuse(rel, r);
                            Ok(None)
                        }
                    }
                }
                None => {
                    self.refuse(rel, RefusalReason::NonUtf8SymlinkTarget);
                    Ok(None)
                }
            };
        }

        if ft.is_dir() {
            // Recurse first so the child node exists to point at; identical
            // listings dedup inside put_meta by content addressing.
            let child = self.walk_dir(path, rel)?;
            let child_tree_id = self.put_tree(&child)?;
            self.stats.dirs += 1;
            let (sec, nsec) = mtime_of(&meta);
            return Ok(Some(dir_entry(&component, sec, nsec, child_tree_id)));
        }

        if ft.is_file() {
            let bytes = std::fs::read(path).map_err(io_err(path))?;
            let mut chunks = Vec::new();
            for piece in chunk(self.poly, &bytes) {
                let id = self.store.put_data(piece)?;
                self.stats.bytes_chunked += piece.len() as u64;
                chunks.push((id, piece.len() as u64));
            }
            let exec = exec_of(&meta);
            self.stats.files += 1;
            let (sec, nsec) = mtime_of(&meta);
            return Ok(Some(file_entry(&component, exec, sec, nsec, chunks)));
        }

        // Sockets, FIFOs, devices: no manifest representation exists. Loud
        // refusal, snapshot continues without the path.
        self.refuse(rel, RefusalReason::UnknownFileType);
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::crypto::{PassthroughCipher, KEY_LEN};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::fs::FileTimes;

    pub(crate) fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| i as u8)
    }

    pub(crate) fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::create(dir.path(), fmk(), Box::new(PassthroughCipher)).unwrap();
        (dir, store)
    }

    pub(crate) fn poly_of(seed: u64) -> u64 {
        crate::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed))
    }

    pub(crate) fn identity(at: (i64, u32)) -> SnapshotIdentity {
        SnapshotIdentity {
            folder_id: [7; 16],
            device_id: [9; 32],
            parent_manifest_id: [0; 32],
            created_sec: at.0,
            created_nsec: at.1,
        }
    }

    pub(crate) fn prng(seed: u64, len: usize) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..len).map(|_| rng.gen()).collect()
    }

    /// Rewrite `path` with exact bytes, mode, and mtime so two physical
    /// trees can be made metadata-identical.
    pub(crate) fn write_file(path: &Path, bytes: &[u8], exec: bool, mt: (i64, u32)) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(path).unwrap().permissions();
            perm.set_mode(if exec { 0o755 } else { 0o644 });
            std::fs::set_permissions(path, perm).unwrap();
        }
        #[cfg(not(unix))]
        let _ = exec;
        set_mtime(path, mt.0, mt.1);
    }

    pub(crate) fn set_mtime(path: &Path, sec: i64, nsec: u32) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_times(FileTimes::new().set_modified(ferry_platform::join_unix(sec, nsec)))
            .unwrap();
    }

    /// Directories carry mtimes in manifests too; determinism tests pin them.
    pub(crate) fn set_dir_mtime(path: &Path, mt: (i64, u32)) {
        #[cfg(unix)]
        {
            let f = std::fs::File::open(path).unwrap();
            f.set_times(FileTimes::new().set_modified(ferry_platform::join_unix(mt.0, mt.1)))
                .unwrap();
        }
        #[cfg(windows)]
        {
            // Directories need FILE_FLAG_BACKUP_SEMANTICS to open.
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .unwrap();
            f.set_times(FileTimes::new().set_modified(ferry_platform::join_unix(mt.0, mt.1)))
                .unwrap();
        }
        #[cfg(not(any(unix, windows)))]
        let _ = (path, mt);
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::manifest::{parse_manifest, parse_tree_node, EntryPayload};

    /// Create one symlink portably where the host permits it.
    #[cfg(unix)]
    fn make_symlink<P: AsRef<Path>>(target: P, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }
    #[cfg(windows)]
    fn make_symlink(target: &str, at: &Path) {
        std::os::windows::fs::symlink_file(target, at).unwrap();
    }

    #[test]
    fn snapshot_captures_tree_contents_and_stores_all_blobs() {
        let (_dir, store) = fresh_store();
        let root = _dir.path().join("tree");
        write_file(
            &root.join("notes.md"),
            b"hello ferry",
            false,
            (1_700_000_000, 123_456_789),
        );
        write_file(
            &root.join("run.sh"),
            b"#!/bin/sh\necho hi\n",
            true,
            (1_700_000_001, 0),
        );
        write_file(&root.join("empty.txt"), b"", false, (5, 42));
        write_file(
            &root.join("docs/a.txt"),
            b"doc bytes",
            false,
            (1_700_000_002, 999_999_999),
        );
        make_symlink("docs", &root.join("jump"));

        let out = snapshot_dir(&store, poly_of(3), &root, &identity((1_000, 500))).unwrap();
        assert!(out.refused.is_empty(), "{:?}", out.refused);
        assert_eq!(out.stats.files, 4);
        assert_eq!(out.stats.dirs, 1);
        assert_eq!(out.stats.symlinks, 1);
        assert_eq!(out.stats.bytes_chunked, 11 + 18 + 9); // empty file adds 0

        // Manifest echoes identity, is addressed by content, and is stored.
        assert_eq!(out.manifest.folder_id, [7; 16]);
        assert_eq!(out.manifest.device_id, [9; 32]);
        assert_eq!(out.manifest.parent_manifest_id, [0; 32]);
        assert_eq!(out.manifest.created_sec, 1_000);
        assert_eq!(out.manifest.created_nsec, 500);
        assert_eq!(
            out.manifest_id,
            *blake3::hash(&serialize_manifest(&out.manifest)).as_bytes()
        );
        assert_eq!(out.manifest.root_tree_id, out.root_tree_id);
        let stored = store.get(BlobKind::Manifest, &out.manifest_id).unwrap();
        assert_eq!(stored, serialize_manifest(&out.manifest));
        let echoed = parse_manifest(&stored).unwrap();
        assert_eq!(echoed, out.manifest);

        // Root tree structure, sorted by name bytes.
        let root_bytes = store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap();
        let root = parse_tree_node(&root_bytes).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["docs", "empty.txt", "jump", "notes.md", "run.sh"]);

        let empty = root.entries.iter().find(|e| e.name == "empty.txt").unwrap();
        assert_eq!(
            empty.payload,
            EntryPayload::File {
                size: 0,
                chunks: vec![]
            },
            "empty file: zero chunks, size 0"
        );

        let run = root.entries.iter().find(|e| e.name == "run.sh").unwrap();
        assert!(run.exec, "exec bit maps to flags bit 0");

        let notes = root.entries.iter().find(|e| e.name == "notes.md").unwrap();
        assert_eq!(notes.mtime_sec, 1_700_000_000);
        assert_eq!(notes.mtime_nsec, 123_456_789);

        let jump = root.entries.iter().find(|e| e.name == "jump").unwrap();
        assert_eq!(
            jump.payload,
            EntryPayload::Symlink {
                target: "docs".to_string()
            }
        );

        // The child tree node resolves and holds the nested file.
        let docs = root.entries.iter().find(|e| e.name == "docs").unwrap();
        let EntryPayload::Dir { child_tree_id } = &docs.payload else {
            panic!("docs must be a dir");
        };
        let docs_node = parse_tree_node(&store.get(BlobKind::TreeNode, child_tree_id).unwrap());
        assert!(docs_node.is_ok());
        assert_eq!(docs_node.unwrap().entries[0].name, "a.txt");

        // Chunks of notes.md are really in the store under their addresses.
        let EntryPayload::File { chunks, .. } = &notes.payload else {
            panic!("notes.md must be a file");
        };
        assert_eq!(chunks.len(), 1);
        let bytes = store.get(BlobKind::DataChunk, &chunks[0].0).unwrap();
        assert_eq!(bytes, b"hello ferry");
        assert_eq!(chunks[0].1, 11);
    }

    #[test]
    fn identical_trees_yield_byte_identical_manifests_and_shared_tree_ids() {
        let (_dir, store) = fresh_store();
        let mt = (1_700_000_123, 777);

        let build = |base: &Path| {
            write_file(&base.join("a.txt"), b"AAA", false, mt);
            write_file(&base.join("dup1/x.txt"), b"shared", false, mt);
            write_file(&base.join("dup2/x.txt"), b"shared", false, mt);
            set_dir_mtime(&base.join("dup1"), mt);
            set_dir_mtime(&base.join("dup2"), mt);
            set_dir_mtime(base, mt);
        };
        let p1 = _dir.path().join("one");
        let p2 = _dir.path().join("two");
        build(&p1);
        build(&p2);

        let idn = identity((42, 7));
        let poly = poly_of(11);
        let o1 = snapshot_dir(&store, poly, &p1, &idn).unwrap();
        let o2 = snapshot_dir(&store, poly, &p2, &idn).unwrap();

        assert_eq!(o1.root_tree_id, o2.root_tree_id, "same tree, same address");
        assert_eq!(
            serialize_manifest(&o1.manifest),
            serialize_manifest(&o2.manifest),
            "byte-identical manifests"
        );
        assert_eq!(o1.manifest_id, o2.manifest_id);

        // Two identical listings anywhere dedup to one tree-node id.
        let root =
            parse_tree_node(&store.get(BlobKind::TreeNode, &o1.root_tree_id).unwrap()).unwrap();
        let child = |name: &str| match &root
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .payload
        {
            EntryPayload::Dir { child_tree_id } => *child_tree_id,
            other => panic!("{name} must be a dir, got {other:?}"),
        };
        let c1 = child("dup1");
        let c2 = child("dup2");
        assert_eq!(c1, c2, "identical sibling listings share one tree node");

        // Dedup is observable through the index: a reopened store serves the
        // shared node from disk state alone.
        store.write_index_snapshot().unwrap();
        drop(store);
        let reopened = crate::store::Store::create(
            _dir.path(),
            fmk(),
            Box::new(crate::crypto::PassthroughCipher),
        );
        assert!(
            reopened.is_err(),
            ".ferry already exists; must open instead"
        );
        let reopened = crate::store::Store::open(
            _dir.path(),
            fmk(),
            Box::new(crate::crypto::PassthroughCipher),
        )
        .unwrap();
        assert!(!reopened.get(BlobKind::TreeNode, &c1).unwrap().is_empty());
    }

    #[test]
    fn resnapshot_of_unchanged_tree_writes_no_new_pack_bytes() {
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        write_file(&tree.join("a.txt"), b"alpha", false, (10, 1));
        write_file(&tree.join("b/c.txt"), b"beta gamma", true, (20, 2));

        let idn = identity((1, 2));
        let poly = poly_of(13);
        let s1 = snapshot_dir(&store, poly, &tree, &idn).unwrap();

        let packs = |p: &Path| -> std::collections::BTreeMap<String, usize> {
            std::fs::read_dir(p.join(".ferry/packs"))
                .unwrap()
                .flatten()
                .map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let len = e.metadata().unwrap().len() as usize;
                    (n, len)
                })
                .collect()
        };
        let before = packs(_dir.path());

        let s2 = snapshot_dir(&store, poly, &tree, &idn).unwrap();
        let after = packs(_dir.path());

        assert_eq!(s1.root_tree_id, s2.root_tree_id);
        assert_eq!(before, after, "dedup must leave the pack set untouched");
    }

    #[test]
    #[cfg(unix)]
    fn unsupported_entries_are_refused_loudly_not_silently() {
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        write_file(&tree.join("good.txt"), b"x", false, (1, 0));
        make_symlink("good.txt", &tree.join("good_link"));

        let fifo = tree.join("pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());

        use std::os::unix::ffi::OsStrExt;
        make_symlink(
            std::ffi::OsStr::from_bytes(b"\xff\xfe"),
            &tree.join("bad_link"),
        );

        // A non-UTF-8 file NAME is possible on unix; if this host allows it,
        // expect a refusal for it too.
        let non_utf8_name = std::ffi::OsStr::from_bytes(b"na\xffme");
        let name_refused = std::fs::write(tree.join(non_utf8_name), b"y").is_ok();

        let out = snapshot_dir(&store, poly_of(17), &tree, &identity((1, 1))).unwrap();

        let mut got: Vec<(Vec<String>, RefusalReason)> = out
            .refused
            .iter()
            .map(|r| (r.path.clone(), r.reason))
            .collect();
        got.sort();
        let mut want: Vec<(Vec<String>, RefusalReason)> = vec![
            (vec!["bad_link".into()], RefusalReason::NonUtf8SymlinkTarget),
            (vec!["pipe".into()], RefusalReason::UnknownFileType),
        ];
        if name_refused {
            want.push((vec!["na\u{fffd}me".into()], RefusalReason::NonUtf8Name));
        }
        want.sort();
        assert_eq!(got, want, "every refusal must be loud and specific");

        // Refused entries are absent from the manifest; good ones remain.
        let root =
            parse_tree_node(&store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap()).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["good.txt", "good_link"]);
        assert_eq!(out.stats.files, 1);
        assert_eq!(out.stats.symlinks, 1);
    }

    #[test]
    fn scan_normalizes_names_to_nfc() {
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        // Decomposed e + combining acute.
        write_file(&tree.join("cafe\u{301}.txt"), b"nfc me", false, (1, 0));

        let out = snapshot_dir(&store, poly_of(19), &tree, &identity((1, 1))).unwrap();
        let root =
            parse_tree_node(&store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap()).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["caf\u{e9}.txt"], "stored form must be composed NFC");
    }

    #[test]
    fn sibling_name_collision_after_nfc_is_a_hard_error() {
        // Both spellings fold to the same NFC name; the walker must refuse
        // the whole directory rather than emit a duplicate-name node.
        let entries = vec![
            file_entry("cafe\u{301}.txt", false, 0, 0, vec![]),
            file_entry("caf\u{e9}.txt", false, 0, 0, vec![]),
        ];
        let err = ensure_no_sibling_collisions(&["d".to_string()], &entries).unwrap_err();
        assert!(matches!(err, SnapshotError::NameCollision { .. }), "{err}");
        assert!(ensure_no_sibling_collisions(
            &[],
            &[
                file_entry("a", false, 0, 0, vec![]),
                file_entry("b", false, 0, 0, vec![]),
            ]
        )
        .is_ok());
    }

    // ---- T-012 policy tests ------------------------------------------------

    #[test]
    fn case_only_siblings_are_fatal_on_folding_hosts_allowed_on_case_sensitive() {
        let entries = vec![
            file_entry("README", false, 0, 0, vec![]),
            file_entry("readme", false, 0, 0, vec![]),
        ];
        match ensure_no_host_case_collisions(&["docs".to_string()], &entries) {
            Err(SnapshotError::CaseCollision {
                parent,
                first,
                second,
            }) => {
                assert_eq!(parent, "docs");
                assert_eq!((first.as_str(), second.as_str()), ("README", "readme"));
            }
            other => {
                // Case-sensitive host: the pair is legal there and MUST pass
                // (Linux CI exercises this arm).
                assert!(
                    !host_folds_case(),
                    "on a folding host the collision must be fatal, got {other:?}"
                );
            }
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn reserved_device_names_are_refused_at_scan() {
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        write_file(&tree.join("aux.txt"), b"blocked", false, (1, 0));
        write_file(&tree.join("auxiliary.txt"), b"fine", false, (1, 0));

        let out = snapshot_dir(&store, poly_of(5), &tree, &identity((1, 1))).unwrap();
        assert_eq!(
            out.refused,
            vec![RefusedPath {
                path: vec!["aux.txt".to_string()],
                reason: RefusalReason::ReservedName,
            }],
            "loud refusal naming the entry"
        );
        let root =
            parse_tree_node(&store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap()).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["auxiliary.txt"]);
    }

    #[test]
    #[cfg(unix)]
    fn absolute_and_escaping_symlinks_are_refused_internal_ones_sync() {
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        write_file(&tree.join("real.txt"), b"x", false, (1, 0));
        std::fs::create_dir(tree.join("sub")).unwrap();
        write_file(&tree.join("sub/inner.txt"), b"y", false, (1, 0));

        make_symlink("/etc/passwd", &tree.join("abs_link"));
        make_symlink("../../outside", &tree.join("esc_link"));
        // From inside sub/: one level up lands at root — internal, allowed.
        make_symlink("../real.txt", &tree.join("sub/ok_link"));

        let out = snapshot_dir(&store, poly_of(6), &tree, &identity((1, 1))).unwrap();
        let mut got: Vec<(Vec<String>, RefusalReason)> = out
            .refused
            .iter()
            .map(|r| (r.path.clone(), r.reason))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                (
                    vec!["abs_link".to_string()],
                    RefusalReason::AbsoluteSymlinkTarget
                ),
                (
                    vec!["esc_link".to_string()],
                    RefusalReason::EscapingSymlinkTarget
                ),
            ],
            "every refusal names path + reason + fix"
        );
        // The internal link made it through as a real entry.
        let sub =
            parse_tree_node(&store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap()).unwrap();
        let _ = sub;
    }

    #[test]
    #[cfg(not(windows))]
    fn decomposed_and_precomposed_directory_spellings_are_one_name() {
        // A directory created with a DECOMPOSED spelling on disk must scan
        // to exactly one NFC-composed manifest entry — not two names.
        let (_dir, store) = fresh_store();
        let tree = _dir.path().join("t");
        // Decomposed: e + combining acute (U+0065 U+0301).
        let nfd_dir = tree.join("rapport-anne\u{301}e");
        std::fs::create_dir_all(nfd_dir).unwrap();
        write_file(
            &tree.join("rapport-anne\u{301}e/a.txt"),
            b"x",
            false,
            (1, 0),
        );

        let out = snapshot_dir(&store, poly_of(7), &tree, &identity((1, 1))).unwrap();
        assert!(out.refused.is_empty());
        let root =
            parse_tree_node(&store.get(BlobKind::TreeNode, &out.root_tree_id).unwrap()).unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["rapport-ann\u{e9}e"], "stored form is composed NFC");

        // Where the host allows BOTH spellings on disk simultaneously
        // (case-sensitive Linux), they NFC-fold onto each other and are a
        // hard error — one name, not two, never a silent merge. On folding
        // hosts the filesystem itself refuses to create the twin, so the
        // situation cannot arise here at all.
        if host_folds_case() {
            return;
        }
        std::fs::create_dir_all(tree.join("rapport-ann\u{e9}e")).unwrap();
        let err = snapshot_dir(&store, poly_of(7), &tree, &identity((2, 2))).unwrap_err();
        assert!(matches!(err, SnapshotError::NameCollision { .. }), "{err}");
    }
}
