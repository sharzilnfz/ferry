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
            let parts = crate::chunker::chunk(poly, &data).unwrap();

            if *size == 0 {
                assert!(parts.is_empty(), "empty file must produce zero chunks");
                continue;
            }

            for part in &parts {
                let id = store.put_data(part).unwrap();
                all_chunk_ids.insert(id);
            }

            let mut rejoined = Vec::with_capacity(data.len());
            for part in &parts {
                let id: BlobId = *blake3::hash(part).as_bytes();
                rejoined.extend_from_slice(&store.get(BlobKind::DataChunk, &id).unwrap());
            }
            assert_eq!(rejoined, data, "session round trip failed at size {size}");

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

            store.flush().unwrap();
            store.write_index_snapshot().unwrap();

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

        let mut rng = StdRng::seed_from_u64(555);
        let base: Vec<u8> = (0..14 * 1024 * 1024)
            .map(|_| (rng.gen::<u8>() & 0x0f) | b'0')
            .collect();

        let id_of = |b: &[u8]| -> BlobId { *blake3::hash(b).as_bytes() };

        let before_parts = chunk_offsets(poly, &base).unwrap();
        let before_ids: Vec<BlobId> = before_parts
            .iter()
            .map(|(o, l)| id_of(&base[*o..*o + l]))
            .collect();
        for (o, l) in &before_parts {
            store.put_data(&base[*o..*o + l]).unwrap();
        }

        let insert_at = 96 * 1024;
        let mut shifted = Vec::with_capacity(base.len() + 1024);
        shifted.extend_from_slice(&base[..insert_at]);
        shifted.extend_from_slice(b"[INSERTED PAYLOAD]".repeat(56).as_slice());
        shifted.extend_from_slice(&base[insert_at..]);

        let after_parts = chunk_offsets(poly, &shifted).unwrap();
        let after_ids: Vec<BlobId> = after_parts
            .iter()
            .map(|(o, l)| id_of(&shifted[*o..*o + l]))
            .collect();

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

        assert_eq!(before_ids.last(), after_ids.last());

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

        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        verify_all_pack_names(folder.path());

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

        let packs_dir = folder.path().join(".ferry/packs");
        for entry in std::fs::read_dir(&packs_dir).unwrap().flatten() {
            let mut bytes = std::fs::read(entry.path()).unwrap();
            if bytes.len() > 28 {
                bytes[27] ^= 0xFF;
                std::fs::write(entry.path(), bytes).unwrap();
            }
        }

        let store = reopen(folder.path());
        let err = store.get(BlobKind::DataChunk, &id).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Pack(crate::pack::PackError::NameMismatch { .. })
            ),
            "{err}"
        );

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

    fn count_ferryindex(index_dir: &std::path::Path) -> usize {
        std::fs::read_dir(index_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "ferryindex"))
            .count()
    }

    #[test]
    fn steady_state_ingest_appends_one_incremental_record_per_sealed_pack() {
        let folder = temp_folder();
        let mut store = new_store(folder.path());
        store.set_seal_target(64 * 1024);
        let index_dir = folder.path().join(".ferry/index");
        assert_eq!(count_ferryindex(&index_dir), 0);

        let mut rng = StdRng::seed_from_u64(909);
        let mut written = Vec::new();
        for _ in 0..6 {
            let bytes: Vec<u8> = (0..80 * 1024).map(|_| rng.gen()).collect();
            let id = store.put_data(&bytes).unwrap();
            written.push((bytes, id));
            store.flush().unwrap();
        }

        let fmk = core::array::from_fn(|i| i as u8);
        let packs = pack_count(folder.path());
        assert!(packs >= 6, "small seal target must force multiple packs");
        assert_eq!(
            count_ferryindex(&index_dir),
            packs,
            "exactly one INDEX record per sealed pack"
        );

        for entry in std::fs::read_dir(&index_dir).unwrap().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let rows = crate::index::open_index_container(&bytes, &fmk, &PassthroughCipher)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.path().display()));
            assert!(!rows.is_empty());
            let pack = rows[0].pack;
            assert!(
                rows.iter().all(|e| e.pack == pack),
                "a record spanning multiple packs is not incremental"
            );
        }

        let reopened = reopen(folder.path());
        for (bytes, id) in &written {
            assert_eq!(&reopened.get(BlobKind::DataChunk, id).unwrap(), bytes);
        }
    }

    fn sealed_test_pack(seed: u8, body_len: usize) -> (BlobId, Vec<u8>, Vec<u8>) {
        let body: Vec<u8> = (0..body_len)
            .map(|i| (i as u8).wrapping_add(seed))
            .collect();
        let id: BlobId = *blake3::hash(&body).as_bytes();
        let entries = vec![FooterEntry {
            kind: BlobKind::DataChunk,
            id,
            plain_off: 0,
            plain_len: body.len() as u64,
        }];
        let salt: [u8; crate::crypto::SALT_LEN] = core::array::from_fn(|i| i as u8);
        let file = seal_pack_bytes(
            crate::format::ContainerKind::PackData,
            &core::array::from_fn(|i| i as u8),
            &salt,
            &body,
            &entries,
            &PassthroughCipher,
        )
        .unwrap();
        (id, file, body)
    }

    #[test]
    fn adopt_pack_folds_locations_in_incrementally_without_any_rebuild() {
        let folder = temp_folder();
        let store = new_store(folder.path());
        let index_dir = folder.path().join(".ferry/index");
        let packs_dir = folder.path().join(".ferry/packs");
        let fmk = core::array::from_fn(|i| i as u8);
        let (id, file, body) = sealed_test_pack(7, 4096);
        let name = pack_name_of(&file);

        assert_eq!(count_ferryindex(&index_dir), 0);
        store.adopt_pack(&name, &file).unwrap();

        assert_eq!(store.get(BlobKind::DataChunk, &id).unwrap(), body);

        assert_eq!(pack_count(folder.path()), 1);
        assert_eq!(count_ferryindex(&index_dir), 1);
        let record = std::fs::read_dir(&index_dir)
            .unwrap()
            .flatten()
            .next()
            .unwrap();
        let rows = crate::index::open_index_container(
            &std::fs::read(record.path()).unwrap(),
            &fmk,
            &PassthroughCipher,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pack, name);

        store.adopt_pack(&name, &file).unwrap();
        assert_eq!(pack_count(folder.path()), 1);
        assert_eq!(store.get(BlobKind::DataChunk, &id).unwrap(), body);

        drop(store);
        let reopened = reopen(folder.path());
        assert_eq!(reopened.get(BlobKind::DataChunk, &id).unwrap(), body);

        let (_id2, file2, _b2) = sealed_test_pack(9, 512);
        let fake = [0xEE; 32];
        let err = reopened.adopt_pack(&fake, &file2).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Pack(crate::pack::PackError::NameMismatch { .. })
            ),
            "{err}"
        );
        assert_eq!(pack_count(folder.path()), 1);
        assert!(!std::fs::read_dir(&packs_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("eeee")));
    }

    #[test]
    fn large_delivered_pack_ingests_incrementally_and_reads_back() {
        let folder = temp_folder();
        let store = new_store(folder.path());

        let (id, file, body) = sealed_test_pack(3, 8 * 1024 * 1024);
        let name = pack_name_of(&file);

        store.adopt_pack(&name, &file).unwrap();
        assert_eq!(store.get(BlobKind::DataChunk, &id).unwrap(), body);

        drop(store);
        let reopened = reopen(folder.path());
        assert_eq!(reopened.get(BlobKind::DataChunk, &id).unwrap(), body);
    }

    #[test]
    fn pack_cache_speeds_repeated_gets_and_survives_reopen() {
        let folder = temp_folder();
        let store = new_store(folder.path());
        let payload = b"cached pack read test payload";
        let id = store.put_data(payload).unwrap();
        store.flush().unwrap();

        assert_eq!(store.pack_cache.len(), 0);

        for _ in 0..5 {
            assert_eq!(store.get(BlobKind::DataChunk, &id).unwrap(), payload);
        }
        assert_eq!(store.pack_cache.len(), 1);

        let reopened = reopen(folder.path());
        assert_eq!(reopened.pack_cache.len(), 0);
        assert_eq!(reopened.get(BlobKind::DataChunk, &id).unwrap(), payload);
        assert_eq!(reopened.pack_cache.len(), 1);
    }

    #[test]
    fn staged_dedup_prevents_duplicate_staging_in_burst() {
        let folder = temp_folder();
        let store = new_store(folder.path());
        let payload = b"duplicate burst data payload";
        let mut ids = Vec::new();
        for _ in 0..10 {
            ids.push(store.put_data(payload).unwrap());
        }

        assert!(ids.iter().all(|&id| id == ids[0]));

        store.flush().unwrap();
        assert_eq!(pack_count(folder.path()), 1);

        let (bpl, entries) = store
            .pack_blob_list(&store.inner.lock().unwrap().locations.packs()[0])
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(bpl, payload.len() as u64);
    }

    #[test]
    fn index_compaction_consolidates_files_and_removes_old() {
        let folder = temp_folder();
        let mut store = new_store(folder.path());
        store.set_seal_target(512);
        let index_dir = folder.path().join(".ferry/index");

        let mut blobs = Vec::new();
        for i in 0..10 {
            let data = vec![i as u8; 600];
            let id = store.put_data(&data).unwrap();
            store.flush().unwrap();
            blobs.push((id, data));
        }

        assert_eq!(count_ferryindex(&index_dir), 10);
        store.compact_index().unwrap();

        assert_eq!(count_ferryindex(&index_dir), 1);

        for (id, data) in &blobs {
            assert_eq!(store.get(BlobKind::DataChunk, id).unwrap(), *data);
        }

        drop(store);
        let reopened = reopen(folder.path());
        assert_eq!(count_ferryindex(&index_dir), 1);
        for (id, data) in &blobs {
            assert_eq!(reopened.get(BlobKind::DataChunk, id).unwrap(), *data);
        }
    }
}

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use thiserror::Error;

use crate::crypto::{CryptoError, PackCipher, KEY_LEN, SALT_LEN};
use crate::format::{hex, BlobId, BlobKind, PackId};
use crate::index::{
    open_index_container, seal_index_container, write_named_atomically, IndexEntry, LocationTable,
};
use crate::pack::{
    pack_name_of, seal_pack_bytes, write_pack_atomically, FooterEntry, PackCache, PackError,
    StagingPools, VerifiedPack, SEAL_TARGET_BYTES, STAGING_OPEN_PACKS,
};

pub const STORE_DIR_NAME: &str = ".ferry";
pub const INDEX_COMPACTION_THRESHOLD: usize = 512;

static REBUILD_INDEX_CALLS: AtomicU64 = AtomicU64::new(0);

static LOCK_HOLD_MAX_US: AtomicU64 = AtomicU64::new(0);
static DEBUG_LOCKS: OnceLock<bool> = OnceLock::new();

pub fn rebuild_index_calls() -> u64 {
    REBUILD_INDEX_CALLS.load(Ordering::Relaxed)
}

pub fn max_lock_hold_us() -> u64 {
    LOCK_HOLD_MAX_US.load(Ordering::Relaxed)
}

fn note_hold(start: Instant) {
    let us = start.elapsed().as_micros() as u64;
    if LOCK_HOLD_MAX_US.fetch_max(us, Ordering::Relaxed) < us
        && *DEBUG_LOCKS.get_or_init(|| std::env::var_os("FERRY_DEBUG").is_some())
        && us > 100_000
    {
        eprintln!("ferry-store: long lock hold {us}us (max so far)");
    }
}

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

pub struct Store {
    root: PathBuf,
    fmk: [u8; KEY_LEN],
    cipher: Box<dyn PackCipher>,
    inner: Mutex<Inner>,

    index_seq: Mutex<()>,

    next_index: AtomicU64,

    dangling_packs: Mutex<HashSet<PackId>>,

    pack_cache: PackCache,

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
            index_seq: Mutex::new(()),
            next_index: AtomicU64::new(0),
            dangling_packs: Mutex::new(HashSet::new()),
            pack_cache: PackCache::default(),
            seal_target: SEAL_TARGET_BYTES,
        })
    }

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
            .filter(|p| p.extension().is_some_and(|x| x == "ferryindex"))
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
            index_seq: Mutex::new(()),
            next_index: AtomicU64::new(0),
            dangling_packs: Mutex::new(HashSet::new()),
            pack_cache: PackCache::default(),
            seal_target: SEAL_TARGET_BYTES,
        })
    }

    pub fn set_seal_target(&mut self, target: usize) {
        self.seal_target = target;
    }

    pub fn store_dir(&self) -> &Path {
        &self.root
    }

    pub fn index_entries(&self) -> Result<Vec<crate::index::IndexEntry>, StoreError> {
        let inner = self.lock()?;
        Ok(inner.locations.iter_sorted())
    }

    pub fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobId, StoreError> {
        if bytes.is_empty() {
            return Err(StoreError::EmptyBlob);
        }
        let id: BlobId = *blake3::hash(bytes).as_bytes();

        let already_present = {
            let inner = self.lock()?;
            let candidates = inner.locations.candidates(kind, &id);
            (!candidates.is_empty() && candidates.iter().any(|e| self.pack_exists(&e.pack)))
                || inner.staging.contains(kind, &id)
        };
        if already_present {
            return Ok(id);
        }

        let mut sealed = {
            let start = Instant::now();
            let mut inner = self.lock()?;
            let sealed = inner.staging.offer(
                kind,
                id,
                bytes,
                self.seal_target,
                STAGING_OPEN_PACKS,
                &mut rand::thread_rng(),
            );
            note_hold(start);
            sealed
        };

        for sp in sealed.drain(..) {
            self.seal_to_disk(sp)?;
        }
        Ok(id)
    }

    pub fn put_data(&self, bytes: &[u8]) -> Result<BlobId, StoreError> {
        self.put_blob(BlobKind::DataChunk, bytes)
    }

    pub fn put_meta(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobId, StoreError> {
        debug_assert!(kind.is_meta(), "put_meta requires a metadata kind");
        self.put_blob(kind, bytes)
    }

    pub fn get(&self, kind: BlobKind, id: &BlobId) -> Result<Vec<u8>, StoreError> {
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

        let mut ordered: Vec<IndexEntry> = candidates;
        ordered.sort_by_key(|e| !self.pack_exists(&e.pack));

        let mut last_err: Option<StoreError> = None;
        for e in ordered {
            if !self.pack_exists(&e.pack) {
                continue;
            }
            let verified = match self.pack_cache.get(&e.pack) {
                Some(vp) => vp,
                None => {
                    let path = self.pack_path(&e.pack);
                    let bytes = match std::fs::read(&path) {
                        Ok(b) => b,
                        Err(err) => {
                            last_err = Some(StoreError::Io(err));
                            continue;
                        }
                    };
                    match VerifiedPack::open(bytes, &e.pack, &self.fmk, self.cipher.as_ref()) {
                        Ok(vp) => {
                            self.pack_cache.insert(vp.clone());
                            vp
                        }
                        Err(err) => {
                            if matches!(err, PackError::Disagreement { .. }) {
                                return Err(StoreError::Pack(err));
                            }
                            last_err = Some(StoreError::Pack(err));
                            continue;
                        }
                    }
                }
            };
            match verified.read_blob(
                self.cipher.as_ref(),
                kind,
                id,
                Some((e.plain_off, e.plain_len)),
            ) {
                Ok(pt) => return Ok(pt),
                Err(err @ PackError::Disagreement { .. }) => {
                    return Err(StoreError::Pack(err));
                }
                Err(other) => last_err = Some(StoreError::Pack(other)),
            }
        }
        Err(last_err.unwrap_or(StoreError::NotFound { kind, id: hex(id) }))
    }

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

    pub fn write_index_snapshot(&self) -> Result<PathBuf, StoreError> {
        let entries = {
            let inner = self.lock()?;
            inner.locations.iter_sorted()
        };
        self.append_index_file(&entries)
    }

    pub fn compact_index(&self) -> Result<PathBuf, StoreError> {
        let index_dir = self.root.join("index");
        let _seq = self.index_seq.lock().map_err(|_| StoreError::Poisoned)?;
        self.do_compact_index_locked(&index_dir)
    }

    fn maybe_compact_index_locked(&self, index_dir: &Path) -> Result<Option<PathBuf>, StoreError> {
        let count = std::fs::read_dir(index_dir)?
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "ferryindex"))
            .count();
        if count >= INDEX_COMPACTION_THRESHOLD {
            Ok(Some(self.do_compact_index_locked(index_dir)?))
        } else {
            Ok(None)
        }
    }

    fn do_compact_index_locked(&self, index_dir: &Path) -> Result<PathBuf, StoreError> {
        let old_files: Vec<PathBuf> = std::fs::read_dir(index_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "ferryindex"))
            .collect();

        let entries = {
            let inner = self.lock()?;
            inner.locations.iter_sorted()
        };

        let mut salt = [0u8; SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
        let file = seal_index_container(&self.fmk, &salt, &entries, self.cipher.as_ref())?;
        let n = self.alloc_index_number(index_dir)?;
        let path = write_named_atomically(
            &self.root.join("tmp"),
            index_dir,
            &format!("{n}.ferryindex"),
            &file,
        )?;

        for old in old_files {
            if old != path {
                let _ = std::fs::remove_file(old);
            }
        }
        Ok(path)
    }

    fn append_index_file(&self, entries: &[IndexEntry]) -> Result<PathBuf, StoreError> {
        let mut salt = [0u8; SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
        let file = seal_index_container(&self.fmk, &salt, entries, self.cipher.as_ref())?;
        let index_dir = self.root.join("index");
        let _seq = self.index_seq.lock().map_err(|_| StoreError::Poisoned)?;
        let n = self.alloc_index_number(&index_dir)?;
        let path = write_named_atomically(
            &self.root.join("tmp"),
            &index_dir,
            &format!("{n}.ferryindex"),
            &file,
        )?;
        self.maybe_compact_index_locked(&index_dir)?;
        Ok(path)
    }

    fn alloc_index_number(&self, index_dir: &Path) -> Result<u64, StoreError> {
        let mut n = self.next_index.load(Ordering::Relaxed);
        if n == 0 {
            n = next_index_number(index_dir)?;
        }
        self.next_index.store(n + 1, Ordering::Relaxed);
        Ok(n)
    }

    pub fn adopt_pack(&self, claimed: &PackId, bytes: &[u8]) -> Result<(), StoreError> {
        let found = crate::pack::pack_name_of(bytes);
        if &found != claimed {
            return Err(StoreError::Pack(PackError::NameMismatch {
                expected: hex(claimed),
                found: hex(&found),
            }));
        }

        let ctx = crate::pack::open_pack(bytes, claimed, &self.fmk, self.cipher.as_ref())?;
        let footer = ctx.entries.clone();

        let dest = self.pack_path(claimed);
        if !dest.is_file() {
            crate::pack::write_pack_atomically(
                &self.root.join("tmp"),
                &self.root.join("packs"),
                bytes,
            )?;
        }

        self.note_pack_written(claimed);
        self.pack_cache.insert(VerifiedPack::from_parts(
            *claimed,
            std::sync::Arc::new(bytes.to_vec()),
            ctx,
        ));

        let entries: Vec<IndexEntry> = footer
            .into_iter()
            .map(|e| IndexEntry {
                kind: e.kind,
                id: e.id,
                pack: *claimed,
                plain_off: e.plain_off,
                plain_len: e.plain_len,
            })
            .collect();
        let start = Instant::now();
        {
            let mut inner = self.lock()?;
            inner.locations.merge(entries.iter().cloned());
            note_hold(start);
        }
        self.append_index_file(&entries)?;
        Ok(())
    }

    pub fn rebuild_index(&self) -> Result<(usize, Vec<String>), StoreError> {
        REBUILD_INDEX_CALLS.fetch_add(1, Ordering::Relaxed);
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

    pub fn put_polynomial(&self, poly: u64) -> Result<BlobId, StoreError> {
        self.put_meta(BlobKind::Polynomial, &poly.to_le_bytes())
    }

    pub fn get_polynomial(&self, id: &BlobId) -> Result<u64, StoreError> {
        let bytes = self.get(BlobKind::Polynomial, id)?;
        let arr: [u8; 8] = bytes.try_into().map_err(|_| {
            StoreError::Manifest(crate::manifest::ManifestError::Corrupt(
                "polynomial record must be exactly 8 bytes",
            ))
        })?;
        Ok(u64::from_le_bytes(arr))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, StoreError> {
        self.inner.lock().map_err(|_| StoreError::Poisoned)
    }

    fn pack_path(&self, pack: &PackId) -> PathBuf {
        self.root.join("packs").join(format!("{}.pack", hex(pack)))
    }

    fn pack_exists(&self, pack: &PackId) -> bool {
        let mut dangling = self
            .dangling_packs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if dangling.contains(pack) {
            return false;
        }
        let exists = self.pack_path(pack).is_file();
        if !exists {
            dangling.insert(*pack);
        }
        exists
    }

    fn note_pack_written(&self, pack: &PackId) {
        self.dangling_packs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pack);
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
        self.note_pack_written(&name);
        let index_entries: Vec<IndexEntry> = entries
            .into_iter()
            .map(|e| IndexEntry {
                kind: e.kind,
                id: e.id,
                pack: name,
                plain_off: e.plain_off,
                plain_len: e.plain_len,
            })
            .collect();
        let start = Instant::now();
        {
            let mut inner = self.lock()?;
            inner.locations.merge(index_entries.iter().cloned());
            note_hold(start);
        }

        self.append_index_file(&index_entries)?;
        Ok(())
    }

    pub(crate) fn pack_blob_list(
        &self,
        pack: &PackId,
    ) -> Result<(u64, Vec<FooterEntry>), PackError> {
        if let Some(vp) = self.pack_cache.get(pack) {
            return Ok((vp.ctx.body_plain_len, vp.ctx.entries));
        }
        let bytes = std::fs::read(self.pack_path(pack))?;
        crate::pack::read_footer(&bytes, pack, &self.fmk, self.cipher.as_ref())
    }

    pub(crate) fn invalidate_pack(&self, pack: &PackId) {
        self.pack_cache.remove(pack);
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
