






































use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_store::manifest::{
    dir_entry, file_entry, serialize_manifest, serialize_tree_node, symlink_entry, EntryPayload,
    RootManifest, TreeEntry, TreeNode,
};
use ferry_store::snapshot::{ensure_no_collisions, RefusedPath};
use ferry_store::store::Store;
use ferry_store::{
    admission::{self, AdmittedKind, ObservedKind},
    BlobId, BlobKind,
};

use crate::error::ScanError;
use crate::ignore::{EntryKind, IgnorePolicy};
use crate::policy::{RelPath, Trigger};
use crate::state::{CachedDir, DirCache};





pub(crate) fn is_store_component(name: &str) -> bool {
    name == ferry_store::store::STORE_DIR_NAME
}



#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    pub trigger: Trigger,
    
    pub files: usize,
    pub dirs: usize,
    pub symlinks: usize,
    
    
    pub bytes_chunked: u64,
    
    pub files_rehashed: usize,
    
    pub dirty_dirs: usize,
    
    
    pub duration: Duration,
}


#[derive(Clone, Debug)]
pub struct ScanOutput {
    pub manifest: RootManifest,
    pub manifest_id: BlobId,
    pub root_tree_id: BlobId,
    pub stats: PassStats,
    pub refused: Vec<RefusedPath>,
}



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



pub(crate) struct Walker<'a> {
    store: &'a Store,
    poly: ferry_store::chunker::ValidatedPoly,
    ignore: &'a dyn IgnorePolicy,
    disk_root: &'a Path,
    cache: &'a mut DirCache,

    stats: PassStats,
    refused: Vec<RefusedPath>,
    
    rebuilt: HashMap<RelPath, BlobId>,
    
    
    read_buf: Vec<u8>,
    chunk_scratch: Vec<u8>,
}

impl<'a> Walker<'a> {
    
    
    
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        store: &'a Store,
        poly: ferry_store::chunker::ValidatedPoly,
        ignore: &'a dyn IgnorePolicy,
        disk_root: &'a Path,
        cache: &'a mut DirCache,
        dirty_closed: &BTreeSet<RelPath>,
        trigger: Trigger,
        identity: &ferry_store::snapshot::SnapshotIdentity,
        prev_root_tree_id: BlobId,
        stats_out: &mut PassStats,
    ) -> Result<Option<ScanOutput>, ScanError> {
        
        
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
            read_buf: vec![0u8; REHASH_READ_BUF],
            chunk_scratch: Vec::with_capacity(ferry_store::chunker::MIN_SIZE * 2),
        };

        for d in order {
            if w.rebuilt.contains_key(d) {
                continue;
            }
            
            
            
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

    
    
    fn rebuild_dir(
        &mut self,
        rel: &RelPath,
        dirty_closed: &BTreeSet<RelPath>,
    ) -> Result<BlobId, ScanError> {
        
        
        if rel.last().is_some_and(|c| is_store_component(c)) {
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
                    
                    
                    
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                return self.splice_absent(rel);
            }
            Err(e) => return Err(Self::io_err(&disk)(e)),
        }
        names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));

        
        
        
        
        let old_node = self.cache.take(rel).map(|c| c.node);
        let old_entries: HashMap<&str, &TreeEntry> = old_node
            .as_ref()
            .map(|n| n.entries.iter().map(|e| (e.name.as_str(), e)).collect())
            .unwrap_or_default();
        let mut entries: Vec<TreeEntry> = Vec::with_capacity(names.len());
        let mut listed_dirs: Vec<RelPath> = Vec::new();

        for name in names {
            let raw = name.as_encoded_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            
            
            
            let component = match admission::admit_name(name.as_os_str()) {
                Ok(c) => c,
                Err(r) => {
                    let mut path = rel.clone();
                    path.push(r.display_name);
                    self.refused.push(RefusedPath {
                        path,
                        reason: r.reason,
                    });
                    continue;
                }
            };
            
            if is_store_component(&component) {
                continue;
            }
            let mut child_rel = rel.clone();
            child_rel.push(component.clone());
            let child_disk = disk.join(&name);

            
            
            
            
            
            let meta = match std::fs::symlink_metadata(&child_disk) {
                Ok(m) => m,
                
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Self::io_err(&child_disk)(e)),
            };
            let ft = meta.file_type();
            
            
            let kind = if ft.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            if self.ignore.ignored(&child_rel, kind) {
                continue;
            }

            
            
            
            let observed = if ft.is_symlink() {
                ObservedKind::Symlink
            } else if ft.is_dir() {
                ObservedKind::Dir
            } else if ft.is_file() {
                ObservedKind::File
            } else {
                ObservedKind::Other
            };
            let link_target = if ft.is_symlink() {
                Some(
                    std::fs::read_link(&child_disk)
                        .map_err(Self::io_err(&child_disk))?
                        .into_os_string(),
                )
            } else {
                None
            };
            let admitted =
                match admission::admit_kind(component, observed, link_target.as_deref(), rel.len())
                {
                    Ok(a) => a,
                    Err(r) => {
                        self.refused.push(RefusedPath {
                            path: child_rel,
                            reason: r.reason,
                        });
                        continue;
                    }
                };
            let component = admitted.component;

            let entry = match admitted.kind {
                AdmittedKind::Symlink { target } => {
                    self.stats.symlinks += 1;
                    symlink_entry(&component, mtime_sec(&meta), mtime_nsec(&meta), &target)
                }
                AdmittedKind::Dir => {
                    let mt = (mtime_sec(&meta), mtime_nsec(&meta));
                    let mtime_changed = match old_entries.get(component.as_str()) {
                        Some(prev) => prev.mtime_sec != mt.0 || prev.mtime_nsec != mt.1,
                        None => true,
                    };
                    let child_id =
                        self.ensure_child(&child_rel, &disk, dirty_closed, mtime_changed)?;
                    listed_dirs.push(child_rel.clone());
                    self.stats.dirs += 1;
                    dir_entry(&component, mt.0, mt.1, child_id)
                }
                AdmittedKind::File => {
                    let size = meta.len();
                    let mt = (mtime_sec(&meta), mtime_nsec(&meta));
                    let exec = live_exec(&meta.permissions());
                    self.stats.files += 1;

                    let chunks = match old_entries.get(component.as_str()) {
                        Some(prev) if reusable(prev, size, mt, exec) => {
                            match &prev.payload {
                                EntryPayload::File { chunks, .. } => chunks.clone(),
                                _ => unreachable!("reusable() guarantees a File payload"),
                            }
                            
                        }
                        _ => self.stream_file_chunks(&child_disk)?,
                    };
                    file_entry(&component, exec, mt.0, mt.1, chunks)
                }
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

    
    
    
    
    
    fn stream_file_chunks(&mut self, path: &Path) -> Result<Vec<(BlobId, u64)>, ScanError> {
        let store = self.store;
        
        
        let mut chunker = ferry_store::chunker::Chunker::new(self.poly.get())?;

        let mut file = std::fs::File::open(path).map_err(Self::io_err(path))?;
        let buf: &mut Vec<u8> = &mut self.read_buf;
        if buf.len() != REHASH_READ_BUF {
            buf.resize(REHASH_READ_BUF, 0);
        }
        
        
        let cur: &mut Vec<u8> = &mut self.chunk_scratch;
        cur.clear();
        let mut chunks: Vec<(BlobId, u64)> = Vec::new();

        loop {
            let n = file.read(buf).map_err(Self::io_err(path))?;
            if n == 0 {
                break;
            }
            let mut eaten = 0usize;
            for len in chunker.feed(&buf[..n]) {
                
                
                let fresh = len - cur.len();
                cur.extend_from_slice(&buf[eaten..eaten + fresh]);
                eaten += fresh;
                let id = store.put_data(cur)?;
                self.stats.bytes_chunked += cur.len() as u64;
                chunks.push((id, cur.len() as u64));
                cur.clear();
            }
            let tail_bytes = &buf[eaten..n];
            cur.extend_from_slice(tail_bytes);
        }

        let tail = chunker.finish();
        debug_assert_eq!(tail, cur.len(), "streamed tail must match retained bytes");
        if tail > 0 {
            let id = store.put_data(cur)?;
            self.stats.bytes_chunked += tail as u64;
            chunks.push((id, tail as u64));
        }
        self.stats.files_rehashed += 1;
        Ok(chunks)
    }

    
    
    fn ensure_child(
        &mut self,
        child_rel: &RelPath,
        _parent_disk: &Path,
        dirty_closed: &BTreeSet<RelPath>,
        mtime_changed: bool,
    ) -> Result<BlobId, ScanError> {
        if let Some(id) = self.rebuilt.get(child_rel) {
            return Ok(*id);
        }
        if !mtime_changed && !dirty_closed.contains(child_rel) {
            if let Some(cached) = self.cache.node(child_rel) {
                return Ok(cached.id);
            }
        }
        
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


const REHASH_READ_BUF: usize = 256 * 1024;




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




fn split_mtime(t: std::time::SystemTime) -> (i64, u32) {
    ferry_platform::split_unix(t)
}
fn mtime_sec(meta: &std::fs::Metadata) -> i64 {
    meta.modified().map_or((0, 0), split_mtime).0
}
fn mtime_nsec(meta: &std::fs::Metadata) -> u32 {
    meta.modified().map_or((0, 0), split_mtime).1
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

    
    
    
    
    struct Fixture {
        _tmp: tempfile::TempDir,
        _store_dir: tempfile::TempDir,
        store: Store,
        poly: ferry_store::chunker::ValidatedPoly,
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
        fn ignored(&self, _rel: &[String], _kind: EntryKind) -> bool {
            false
        }
    }

    fn p(parts: &[&str]) -> RelPath {
        parts.iter().map(std::string::ToString::to_string).collect()
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

        
        assert_eq!(
            out.root_tree_id,
            fx.scratch_root_id(),
            "incremental splice must reproduce a from-scratch snapshot"
        );

        
        
        
        let inc_diff = fx.diff_since_baseline(out.root_tree_id);
        let scratch_diff = fx.diff_since_baseline(fx.scratch_root_id());
        assert_eq!(inc_diff, scratch_diff);
        
        
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
        
        
        
        let expect_meta = usize::from(cfg!(unix));
        assert_eq!(inc_diff.metadata_modified.len(), expect_meta);
        if cfg!(unix) {
            assert_eq!(inc_diff.metadata_modified[0].path, p(&["sub", "a.txt"]));
        }
    }

    #[test]
    fn short_circuit_zero_change_pass_hashes_nothing() {
        let mut fx = Fixture::new("t2");
        write_file(&fx.root.join("x.bin"), &prng(1, 4096), false, (1, 0));
        write_file(&fx.root.join("d/y.bin"), &prng(2, 8192), true, (2, 0));
        fx.full_scan();

        
        
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

        
        let mut evil = original.clone();
        evil[0] ^= 0xff;
        evil[512] ^= 0x55;
        write_file(&victim, &evil, false, (100, 500));

        
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
            fn ignored(&self, rel: &[String], _kind: EntryKind) -> bool {
                rel.first().map(std::string::String::as_str) == Some("secrets")
            }
        }
        let mut fx = Fixture::new("t4");
        write_file(&fx.root.join("open.txt"), b"visible", false, (1, 0));
        write_file(&fx.root.join("secrets/key.pem"), b"hidden", false, (2, 0));
        fx.full_scan();

        
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
                
                let node = &fx.cache.node(&p(&["doomed"])).unwrap().node;
                node.entries.is_empty()
            }
        );
        assert!(fx.cache.node(&p(&["doomed", "deep"])).is_none());
        
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
        
        
        
        let mut fx = Fixture::new("t12policy");
        write_file(&fx.root.join("keep.txt"), b"k", false, (1, 0));
        
        
        #[cfg(unix)]
        write_file(&fx.root.join("aux.txt"), b"reserved", false, (2, 0));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", fx.root.join("abs_link")).unwrap();
            std::os::unix::fs::symlink("keep.txt", fx.root.join("ok_link")).unwrap();
        }
        let first = fx.full_scan_with_ledger();

        use ferry_store::snapshot::RefusalReason;
        
        
        
        
        assert_eq!(
            first
                .iter()
                .any(|r| r.reason == RefusalReason::ReservedName),
            cfg!(unix)
        );
        #[cfg(unix)]
        assert!(first
            .iter()
            .any(|r| r.reason == RefusalReason::AbsoluteSymlinkTarget));

        
        
        write_file(&fx.root.join("keep.txt"), b"k2", false, (3, 0));
        let out = fx.incremental_expect(&[p(&[])]);
        let root = ferry_store::manifest::parse_tree_node(
            &fx.store
                .get(ferry_store::BlobKind::TreeNode, &out.root_tree_id)
                .unwrap(),
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
    #[cfg(unix)]
    fn adversarial_entries_produce_identical_trees_and_ledgers_in_both_walkers() {
        
        
        
        
        
        use std::os::unix::ffi::OsStrExt;

        let mut fx = Fixture::new("t11adv");
        let sub = fx.root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        write_file(&fx.root.join("keep.txt"), b"k", false, (1, 0));
        write_file(&sub.join("aux.txt"), b"reserved", false, (2, 0));
        std::os::unix::fs::symlink("../../outside", sub.join("esc_link")).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(sub.join("pipe"))
            .status()
            .unwrap()
            .success());
        
        
        let non_utf8 = std::ffi::OsStr::from_bytes(b"na\xffme");
        let name_refused = std::fs::write(sub.join(non_utf8), b"y").is_ok();

        let ledger = fx.full_scan_with_ledger();
        let inc_id = fx.prev_root_tree_id;

        use ferry_store::snapshot::{snapshot_dir, RefusalReason};
        let scratch = snapshot_dir(&fx.store, fx.poly, &fx.root, &identity((3, 0))).unwrap();

        assert_eq!(
            inc_id, scratch.root_tree_id,
            "incremental == from-scratch by construction of the shared gate"
        );

        let mut want: Vec<(Vec<String>, RefusalReason)> = vec![
            (
                vec!["sub".into(), "aux.txt".into()],
                RefusalReason::ReservedName,
            ),
            (
                vec!["sub".into(), "esc_link".into()],
                RefusalReason::EscapingSymlinkTarget,
            ),
            (
                vec!["sub".into(), "pipe".into()],
                RefusalReason::UnknownFileType,
            ),
        ];
        if name_refused {
            want.push((
                vec!["sub".into(), "na\u{fffd}me".into()],
                RefusalReason::NonUtf8Name,
            ));
        }
        want.sort();

        let mut got: Vec<_> = ledger.iter().map(|r| (r.path.clone(), r.reason)).collect();
        got.sort();
        assert_eq!(got, want, "incremental walker's ledger");

        let mut scr: Vec<_> = scratch
            .refused
            .iter()
            .map(|r| (r.path.clone(), r.reason))
            .collect();
        scr.sort();
        assert_eq!(scr, want, "from-scratch snapshot's ledger");
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
