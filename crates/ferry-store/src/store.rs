//! The store: the API over `.ferry/` that turns bytes into indexed,
//! pack-backed blobs and back.
//!
//! Write path (`docs/store-format.md`): blobs land in up-to-8 open staging
//! packs per kind with CSPRNG assignment and a 16 MiB seal target; sealing
//! serializes, encrypts (stubbed cipher for v0), writes to `tmp/`, fsyncs,
//! and renames into `packs/` atomically. Readers only ever see complete
//! packs.
//!
//! Concurrency: staging pools and the in-memory location table are mutexed;
//! pack renames are atomic; index appends are new files, never rewrites. Any
//! number of threads may put/get concurrently through a shared `&Store`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::{chunk_offsets, generate_polynomial, MAX_SIZE, MIN_SIZE};
    use crate::crypto::PassthroughCipher;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashSet;
    use std::sync::Arc;

    fn temp_folder() -> tempfile::TempDir {
        // Empty folder; Store::create makes .ferry itself.
        tempfile::tempdir().unwrap()
    }

    fn new_store(folder: &std::path::Path) -> Store {
        Store::create(
            folder,
            core::array::from_fn(|i| i as u8),
            Box::new(PassthroughCipher),
        )
        .unwrap()
    }

    fn reopen(folder: &std::path::Path) -> Store {
        Store::open(
            folder,
            core::array::from_fn(|i| i as u8),
            Box::new(PassthroughCipher),
        )
        .unwrap()
    }

    /// Full acceptance path: arbitrary bytes -> chunks -> store -> reassembly
    /// identical, across sizes spanning empty to multi-MiB.
    #[test]
    fn round_trip_property_bytes_to_chunks_to_blobs() {
        let folder = temp_folder();
        let poly = generate_polynomial(&mut StdRng::seed_from_u64(77));
        let store = new_store(folder.path());

        let sizes = [
            0usize,
            1,
            63,
            64,
            1024,
            MIN_SIZE - 1,
            MIN_SIZE,
            MIN_SIZE + 12345,
            MAX_SIZE - 1,
            MAX_SIZE,
            2 * 1024 * 1024 + 777_777,
        ];
        let mut rng = StdRng::seed_from_u64(4242);
        let mut all_chunk_ids: HashSet<BlobId> = HashSet::new();

        for (i, size) in sizes.iter().enumerate() {
            let data: Vec<u8> = (0..*size).map(|_| rng.gen()).collect();
            let parts = crate::chunker::chunk(poly, &data);

            if *size == 0 {
                assert!(parts.is_empty(), "empty file must produce zero chunks");
                continue;
            }

            for part in &parts {
                let id = store.put_data(part).unwrap();
                all_chunk_ids.insert(id);
            }

            // Reassembly from THIS session (staging fallback included).
            let mut rejoined = Vec::with_capacity(data.len());
            for part in &parts {
                let id: BlobId = *blake3::hash(part).as_bytes();
                rejoined.extend_from_slice(&store.get(BlobKind::DataChunk, &id).unwrap());
            }
            assert_eq!(rejoined, data, "session round trip failed at size {size}");

            // Metadata blob rides PACK_META.
            let tree = crate::manifest::TreeNode {
                entries: vec![crate::manifest::file_entry(
                    &format!("f{i}"),
                    false,
                    0,
                    0,
                    parts
                        .iter()
                        .map(|p| (*blake3::hash(p).as_bytes(), p.len() as u64))
                        .collect(),
                )],
            };
            let tree_bytes = crate::manifest::serialize_tree_node(&tree);
            store.put_meta(BlobKind::TreeNode, &tree_bytes).unwrap();

            // Seal and snapshot after each file so later reopen checks see
            // cumulative disk state.
            store.flush().unwrap();
            store.write_index_snapshot().unwrap();

            // Fresh instance reads everything back through disk state only.
            let reopened = reopen(folder.path());
            let mut rejoined = Vec::new();
            for part in &parts {
                let id: BlobId = *blake3::hash(part).as_bytes();
                rejoined.extend_from_slice(&reopened.get(BlobKind::DataChunk, &id).unwrap());
            }
            assert_eq!(rejoined, data, "reopened round trip failed at size {size}");
            assert_eq!(
                reopened
                    .get(BlobKind::TreeNode, blake3::hash(&tree_bytes).as_bytes())
                    .unwrap(),
                tree_bytes
            );
        }
        assert!(!all_chunk_ids.is_empty());
    }

    #[test]
    fn dedup_shifted_insertion_keeps_downstream_chunks_stable() {
        let folder = temp_folder();
        let poly = generate_polynomial(&mut StdRng::seed_from_u64(78));
        let store = new_store(folder.path());

        // ~14 MiB of structured pseudo-random content (large enough that
        // many chunks exist downstream of the insertion point).
        let mut rng = StdRng::seed_from_u64(555);
        let base: Vec<u8> = (0..14 * 1024 * 1024)
            .map(|_| (rng.gen::<u8>() & 0x0f) | b'0')
            .collect();

        let id_of = |b: &[u8]| -> BlobId { *blake3::hash(b).as_bytes() };

        let before_parts = chunk_offsets(poly, &base);
        let before_ids: Vec<BlobId> = before_parts
            .iter()
            .map(|(o, l)| id_of(&base[*o..*o + l]))
            .collect();
        for (o, l) in &before_parts {
            store.put_data(&base[*o..*o + l]).unwrap();
        }

        // Insert 1 KiB near the start, well before most boundaries.
        let insert_at = 96 * 1024;
        let mut shifted = Vec::with_capacity(base.len() + 1024);
        shifted.extend_from_slice(&base[..insert_at]);
        shifted.extend_from_slice(b"[INSERTED PAYLOAD]".repeat(56).as_slice());
        shifted.extend_from_slice(&base[insert_at..]);

        let after_parts = chunk_offsets(poly, &shifted);
        let after_ids: Vec<BlobId> = after_parts
            .iter()
            .map(|(o, l)| id_of(&shifted[*o..*o + l]))
            .collect();

        // How many chunks AFTER the insertion point are byte-identical?
        let downstream_before = &before_ids[1..];
        let downstream_after = &after_ids[1..];
        let shared: HashSet<&BlobId> = downstream_before
            .iter()
            .filter(|id| downstream_after.contains(id))
            .collect();
        assert!(
            shared.len() as f64 >= (downstream_before.len() as f64) * 0.5,
            "expected most downstream chunks to survive an early insertion: \
             {}/{}",
            shared.len(),
            downstream_before.len()
        );
        // The tail chunk specifically must be unchanged.
        assert_eq!(before_ids.last(), after_ids.last());

        // Storing the shifted version adds few NEW blobs: most were deduped.
        let known: HashSet<BlobId> = before_ids.iter().copied().collect();
        let new_blobs = after_ids.iter().filter(|id| !known.contains(*id)).count();
        assert!(
            new_blobs <= 4,
            "early insertion should create only local churn, saw {new_blobs} new"
        );
    }

    #[test]
    fn identical_content_never_grows_the_pack_set() {
        let folder = temp_folder();
        let store = new_store(folder.path());
        let payload = b"the same bytes over and over";
        let a = store.put_data(payload).unwrap();
        store.flush().unwrap();
        let packs_after_first = pack_count(folder.path());
        let b = store.put_data(payload).unwrap();
        store.flush().unwrap();
        assert_eq!(a, b);
        assert_eq!(pack_count(folder.path()), packs_after_first);
    }

    #[test]
    fn concurrent_writers_produce_a_consistent_store() {
        let folder = temp_folder();
        let store = Arc::new(new_store(folder.path()));

        let writers = 8usize;
        let blobs_per_writer = 20usize;
        let mut handles = Vec::new();
        for w in 0..writers {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(1000 + w as u64);
                let mut ids = Vec::new();
                for i in 0..blobs_per_writer {
                    let len = 1 + (rng.gen::<usize>() % 4096);
                    let bytes: Vec<u8> = (0..len).map(|_| ((w * 31 + i) % 251) as u8).collect();
                    let id = if i % 4 == 0 {
                        let meta = format!("meta-{w}-{i}").into_bytes();
                        store.put_meta(BlobKind::TreeNode, &meta).unwrap()
                    } else {
                        store.put_data(&bytes).unwrap()
                    };
                    ids.push((
                        if i % 4 == 0 {
                            BlobKind::TreeNode
                        } else {
                            BlobKind::DataChunk
                        },
                        id,
                    ));
                }
                ids
            }));
        }
        let per_thread: Vec<Vec<(BlobKind, BlobId)>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        // End-of-burst rule: seal everything.
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        // Every pack on disk verifies against its own name.
        verify_all_pack_names(folder.path());

        // Every written blob is readable through a fresh store.
        let reopened = Arc::new(reopen(folder.path()));
        let mut all_ids = Vec::new();
        for thread_ids in &per_thread {
            for (kind, id) in thread_ids {
                reopened.get(*kind, id).unwrap_or_else(|e| {
                    panic!(
                        "blob {} unreadable after concurrent write: {e}",
                        crate::format::hex(id)
                    )
                });
                all_ids.push((*kind, *id));
            }
        }
        assert_eq!(all_ids.len(), writers * blobs_per_writer);

        // Concurrent readers racing a writer must also hold up.
        let reader_handles: Vec<_> = (0..4)
            .map(|_| {
                let s = Arc::clone(&reopened);
                let ids = all_ids.clone();
                std::thread::spawn(move || {
                    for (kind, id) in &ids {
                        s.get(*kind, id).unwrap();
                    }
                })
            })
            .collect();
        for h in reader_handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn corrupted_pack_is_rejected_through_public_api() {
        let folder = temp_folder();
        let store = new_store(folder.path());
        let id = store.put_data(b"some content").unwrap();
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();
        drop(store);

        // Corrupt every pack's first body byte.
        let packs_dir = folder.path().join(".ferry/packs");
        for entry in std::fs::read_dir(&packs_dir).unwrap().flatten() {
            let mut bytes = std::fs::read(entry.path()).unwrap();
            if bytes.len() > 28 {
                bytes[27] ^= 0xFF;
                std::fs::write(entry.path(), bytes).unwrap();
            }
        }

        // The reopened store resolves the location, verifies the name first,
        // and refuses without decrypting anything.
        let store = reopen(folder.path());
        let err = store.get(BlobKind::DataChunk, &id).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Pack(crate::pack::PackError::NameMismatch { .. })
            ),
            "{err}"
        );

        // Recovery: rebuilding the index from packs cannot resurrect a
        // corrupt pack either -- it is reported and skipped.
        let (_, skipped) = store.rebuild_index().unwrap();
        assert_eq!(skipped.len(), 1);
    }

    fn pack_count(folder: &std::path::Path) -> usize {
        std::fs::read_dir(folder.join(".ferry/packs"))
            .unwrap()
            .count()
    }

    fn verify_all_pack_names(folder: &std::path::Path) {
        for entry in std::fs::read_dir(folder.join(".ferry/packs"))
            .unwrap()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            let claimed = crate::format::unhex::<32>(name.trim_end_matches(".pack"))
                .expect("pack names are hex");
            let bytes = std::fs::read(entry.path()).unwrap();
            assert_eq!(*blake3::hash(&bytes).as_bytes(), claimed, "{name}");
        }
    }
}

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;

use crate::crypto::{CryptoError, PackCipher, KEY_LEN, SALT_LEN};
use crate::format::{hex, BlobId, BlobKind, PackId};
use crate::index::{
    open_index_container, seal_index_container, write_named_atomically, IndexEntry, LocationTable,
};
use crate::pack::{
    pack_name_of, seal_pack_bytes, write_pack_atomically, FooterEntry, PackError, StagingPools,
    SEAL_TARGET_BYTES, STAGING_OPEN_PACKS,
};

pub const STORE_DIR_NAME: &str = ".ferry";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("empty blobs cannot exist in the store (an empty file has zero chunks)")]
    EmptyBlob,
    #[error("unknown blob kind")]
    UnknownBlobKind,
    #[error("blob not found: {kind:?} {id}")]
    NotFound { kind: BlobKind, id: String },
    #[error("{0}")]
    Pack(#[from] PackError),
    #[error("{0}")]
    Index(#[from] crate::index::IndexError),
    #[error("{0}")]
    Crypto(#[from] CryptoError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("poisoned lock while another writer panicked")]
    Poisoned,
}

struct Inner {
    staging: StagingPools,
    locations: LocationTable,
}

/// Handle to one `.ferry/` store directory.
pub struct Store {
    root: PathBuf, // the .ferry directory itself
    fmk: [u8; KEY_LEN],
    cipher: Box<dyn PackCipher>,
    inner: Mutex<Inner>,
    /// Staging seal target. Production default is the spec's 16 MiB
    /// (`SEAL_TARGET_BYTES`); tests shrink it to force many small packs.
    seal_target: usize,
}

impl Store {
    fn paths(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let store_dir = root.join(STORE_DIR_NAME);
        (
            store_dir.clone(),
            store_dir.join("packs"),
            store_dir.join("index"),
            store_dir.join("tmp"),
        )
    }

    /// Create a fresh store layout. Fails if `.ferry` already exists.
    pub fn create(
        folder_root: &Path,
        fmk: [u8; KEY_LEN],
        cipher: Box<dyn PackCipher>,
    ) -> Result<Store, StoreError> {
        let (store_dir, packs, index, tmp) = Self::paths(folder_root);
        std::fs::create_dir(&store_dir)?;
        std::fs::create_dir(&packs)?;
        std::fs::create_dir(&index)?;
        std::fs::create_dir(&tmp)?;
        Ok(Store {
            root: store_dir,
            fmk,
            cipher,
            inner: Mutex::new(Inner {
                staging: StagingPools::new(),
                locations: LocationTable::default(),
            }),
            seal_target: SEAL_TARGET_BYTES,
        })
    }

    /// Open an existing store, loading the union of its index files.
    pub fn open(
        folder_root: &Path,
        fmk: [u8; KEY_LEN],
        cipher: Box<dyn PackCipher>,
    ) -> Result<Store, StoreError> {
        let (store_dir, _, index_dir, _) = Self::paths(folder_root);
        if !store_dir.is_dir() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no {} under {}", STORE_DIR_NAME, folder_root.display()),
            )));
        }
        let mut locations = LocationTable::default();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&index_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "ferryindex").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            let bytes = std::fs::read(&f)?;
            let entries = open_index_container(&bytes, &fmk, cipher.as_ref())?;
            locations.merge(entries);
        }
        Ok(Store {
            root: store_dir,
            fmk,
            cipher,
            inner: Mutex::new(Inner {
                staging: StagingPools::new(),
                locations,
            }),
            seal_target: SEAL_TARGET_BYTES,
        })
    }

    /// Override the staging seal target (tests only; production keeps the
    /// spec's 16 MiB default). Must be called before any puts.
    pub fn set_seal_target(&mut self, target: usize) {
        self.seal_target = target;
    }

    /// The `.ferry` directory backing this store.
    pub fn store_dir(&self) -> &Path {
        &self.root
    }

    /// Address one blob by content. Dedup: when the location table already
    /// knows the id AND at least one candidate pack still exists, nothing is
    /// staged or rewritten (content addressing guarantees identical bytes).
    pub fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobId, StoreError> {
        if bytes.is_empty() {
            return Err(StoreError::EmptyBlob);
        }
        let id: BlobId = *blake3::hash(bytes).as_bytes();

        let packs_exist = {
            let inner = self.lock()?;
            let candidates = inner.locations.candidates(kind, &id);
            !candidates.is_empty() && candidates.iter().any(|e| self.pack_path(&e.pack).is_file())
        };
        if packs_exist {
            return Ok(id);
        }

        let mut sealed = {
            let mut inner = self.lock()?;
            inner.staging.offer(
                kind,
                id,
                bytes,
                self.seal_target,
                STAGING_OPEN_PACKS,
                &mut rand::thread_rng(),
            )
        };
        // offer() returns any packs sealed early by the overflow rule.
        for sp in sealed.drain(..) {
            self.seal_to_disk(sp)?;
        }
        Ok(id)
    }

    /// Convenience for data chunks.
    pub fn put_data(&self, bytes: &[u8]) -> Result<BlobId, StoreError> {
        self.put_blob(BlobKind::DataChunk, bytes)
    }

    /// Convenience for metadata blobs (tree nodes, manifests, polynomial).
    pub fn put_meta(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobId, StoreError> {
        debug_assert!(kind.is_meta(), "put_meta requires a metadata kind");
        self.put_blob(kind, bytes)
    }

    /// Read one blob following the normative procedure, trying every known
    /// location whose pack still exists. A footer/index DISAGREEMENT stops
    /// the whole read immediately ("trust neither"), per spec.
    pub fn get(&self, kind: BlobKind, id: &BlobId) -> Result<Vec<u8>, StoreError> {
        // Read-your-writes: blobs still sitting in open staging packs.
        {
            let inner = self.lock()?;
            if let Some(staged) = inner.staging.staged_bytes(kind, id) {
                let found = *blake3::hash(&staged).as_bytes();
                if &found != id {
                    return Err(StoreError::Pack(PackError::HashMismatch {
                        expected: hex(id),
                        found: hex(&found),
                    }));
                }
                return Ok(staged);
            }
        }

        let candidates = {
            let inner = self.lock()?;
            inner.locations.candidates(kind, id)
        };
        if candidates.is_empty() {
            return Err(StoreError::NotFound { kind, id: hex(id) });
        }
        // Spec resolution rule: prefer any entry whose pack still exists.
        let mut ordered: Vec<IndexEntry> = candidates;
        ordered.sort_by_key(|e| !self.pack_path(&e.pack).is_file());

        let mut last_err: Option<StoreError> = None;
        for e in ordered {
            let path = self.pack_path(&e.pack);
            if !path.is_file() {
                continue;
            }
            match std::fs::read(&path)
                .map_err(StoreError::Io)
                .and_then(|bytes| {
                    crate::pack::read_blob(
                        &bytes,
                        &e.pack,
                        &self.fmk,
                        self.cipher.as_ref(),
                        kind,
                        id,
                        Some((e.plain_off, e.plain_len)),
                    )
                    .map_err(StoreError::Pack)
                }) {
                Ok(pt) => return Ok(pt),
                Err(err @ StoreError::Pack(PackError::Disagreement { .. })) => {
                    // Trust neither source; abort the whole read now.
                    return Err(err);
                }
                Err(other) => last_err = Some(other),
            }
        }
        Err(last_err.unwrap_or(StoreError::NotFound { kind, id: hex(id) }))
    }

    /// End-of-burst rule: seal every open staging pack (empties discarded).
    /// Returns how many pack files landed.
    pub fn flush(&self) -> Result<usize, StoreError> {
        let drained = {
            let mut inner = self.lock()?;
            inner.staging.drain_all()
        };
        let n = drained.len();
        for sp in drained {
            self.seal_to_disk(sp)?;
        }
        Ok(n)
    }

    /// Append a fresh INDEX container capturing every known location. The
    /// union across files is the index, so appends never rewrite history.
    pub fn write_index_snapshot(&self) -> Result<PathBuf, StoreError> {
        let entries = {
            let inner = self.lock()?;
            inner.locations.iter_sorted()
        };
        let mut salt = [0u8; SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
        let file = seal_index_container(&self.fmk, &salt, &entries, self.cipher.as_ref())?;
        let index_dir = self.root.join("index");
        let n = next_index_number(&index_dir)?;
        Ok(write_named_atomically(
            &self.root.join("tmp"),
            &index_dir,
            &format!("{n}.ferryindex"),
            &file,
        )?)
    }

    /// Recovery: rebuild the location table straight from the packs and
    /// append it as a fresh index file (restic-style `rebuild`).
    pub fn rebuild_index(&self) -> Result<(usize, Vec<String>), StoreError> {
        let (entries, skipped) = crate::index::rebuild_entries(
            &self.root.join("packs"),
            &self.fmk,
            self.cipher.as_ref(),
        )?;
        let count = entries.len();
        {
            let mut inner = self.lock()?;
            inner.locations.merge(entries);
        }
        self.write_index_snapshot()?;
        Ok((count, skipped))
    }

    /// Store the folder's chunker polynomial as blob_kind 0x04 inside a
    /// PACK_META (protected like a key, per the folder layout section).
    pub fn put_polynomial(&self, poly: u64) -> Result<BlobId, StoreError> {
        self.put_meta(BlobKind::Polynomial, &poly.to_le_bytes())
    }

    /// Read back the polynomial record (8-byte LE u64).
    pub fn get_polynomial(&self, id: &BlobId) -> Result<u64, StoreError> {
        let bytes = self.get(BlobKind::Polynomial, id)?;
        let arr: [u8; 8] = bytes.try_into().map_err(|_| {
            StoreError::Manifest(crate::manifest::ManifestError::Corrupt(
                "polynomial record must be exactly 8 bytes",
            ))
        })?;
        Ok(u64::from_le_bytes(arr))
    }

    // --- internals ---

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, StoreError> {
        self.inner.lock().map_err(|_| StoreError::Poisoned)
    }

    fn pack_path(&self, pack: &PackId) -> PathBuf {
        self.root.join("packs").join(format!("{}.pack", hex(pack)))
    }

    fn seal_to_disk(&self, sp: crate::pack::StagingPack) -> Result<(), StoreError> {
        let entries: Vec<FooterEntry> = sp.entries.clone();
        let file = seal_pack_bytes(
            sp.kind,
            &self.fmk,
            &sp.salt,
            &sp.body,
            &entries,
            self.cipher.as_ref(),
        )?;
        let name = write_pack_atomically(&self.root.join("tmp"), &self.root.join("packs"), &file)?;
        debug_assert_eq!(name, pack_name_of(&file));
        let mut inner = self.lock()?;
        inner
            .locations
            .merge(entries.into_iter().map(|e| IndexEntry {
                kind: e.kind,
                id: e.id,
                pack: name,
                plain_off: e.plain_off,
                plain_len: e.plain_len,
            }));
        Ok(())
    }

    /// Expose footer entries of a pack for GC callers.
    pub(crate) fn pack_blob_list(
        &self,
        pack: &PackId,
    ) -> Result<(u64, Vec<FooterEntry>), PackError> {
        let bytes = std::fs::read(self.pack_path(pack))?;
        crate::pack::read_footer(&bytes, pack, &self.fmk, self.cipher.as_ref())
    }

    pub(crate) fn packs_dir(&self) -> PathBuf {
        self.root.join("packs")
    }
}

fn next_index_number(index_dir: &Path) -> Result<u64, StoreError> {
    let mut max = 0u64;
    for entry in std::fs::read_dir(index_dir)?.flatten() {
        let stem = entry
            .path()
            .file_stem()
            .map(|s| s.to_string_lossy().to_string());
        if let Some(n) = stem.and_then(|s| s.parse::<u64>().ok()) {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}
