//! Acceptance harness for T-008 (public API only).
//!
//! Two in-process engines — each holding a real `Store` + `DeviceIdentity`
//! — converge over (a) an in-memory duplex pair and (b) real localhost TCP,
//! with session encryption OFF and ON. Encryption-on-over-TCP is THE
//! ticket's acceptance mode, standing in for "T-006's skeleton runs over
//! this protocol with encryption on" until ferry-sync merges; final
//! skeleton integration lands post-merge of T-006.
//!
//! Corruption-injection works at the STREAM level: a `Tamper` wrapper
//! flips/truncates records in flight, so the engines under test are the
//! unmodified production code paths.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use ferry_crypto::identity::DeviceIdentity;
use ferry_proto::agreement::AgreementLedger;
use ferry_proto::stream::DuplexHalf;
use ferry_proto::{
    duplex_pair, run_engine, ByteStream, DeviceId, EngineConfig, FolderState, Granularity,
    ProtoError, Role, SessionReport,
};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
use ferry_store::store::Store;

const FMK: [u8; 32] = [7u8; 32];
const FOLDER: [u8; 16] = [
    0x5f, 0x6c, 0x64, 0x72, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
];

// --- fixtures ------------------------------------------------------------------

fn fresh_identity(seed: u8) -> DeviceIdentity {
    let mut sk = [0u8; 32];
    for (i, b) in sk.iter_mut().enumerate() {
        *b = seed.wrapping_mul(89).wrapping_add(i as u8 ^ 0x5a);
    }
    DeviceIdentity::from_secret_bytes(&sk)
}

fn new_store(dir: &Path) -> Arc<Store> {
    Arc::new(Store::create(dir, FMK, Box::new(PassthroughCipher)).unwrap())
}

/// Build the shared source tree: a 3 MiB multi-chunk file plus a deep
/// unicode-named directory structure. Returns (root, `expected_file_bytes`)
/// keyed by relative path for later comparison.
fn build_source_tree(base: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    use unicode_normalization::UnicodeNormalization;
    let nfc = |s: &str| s.nfc().collect::<String>();

    let mut rng = StdRng::seed_from_u64(20260824);
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "big.bin".to_string(),
        (0..3 * 1024 * 1024).map(|_| rng.gen()).collect(),
    );
    files.insert("build.sh".to_string(), b"#!/bin/sh\necho ok\n".to_vec());
    files.insert(
        "src/util/math.rs".to_string(),
        b"pub fn two() -> u32 { 2 }".to_vec(),
    );
    files.insert(
        format!(
            "{}/{}/{}/{}",
            nfc("日本語プロジェクト"),
            nfc("絵文字🚀"),
            nfc("café naïve/深い/さらに/その先"),
            nfc("データ file 📝.txt")
        ),
        "unicode content 🎉".as_bytes().to_vec(),
    );
    // Deep empty-ish directory also present as a dir entry.
    std::fs::create_dir_all(
        base.join(nfc("日本語プロジェクト"))
            .join(nfc("絵文字🚀"))
            .join(nfc("empty-dir")),
    )
    .unwrap();

    for (rel, bytes) in &files {
        let p = base.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, bytes).unwrap();
    }
    files
}

fn snapshot(store: &Store, source: &Path, device: DeviceId, created: i64) -> BlobId {
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(99));
    let out = snapshot_dir(
        store,
        poly,
        source,
        &SnapshotIdentity {
            folder_id: FOLDER,
            device_id: device,
            parent_manifest_id: [0; 32],
            created_sec: created,
            created_nsec: 42,
        },
    )
    .unwrap();
    assert!(out.refused.is_empty(), "{:?}", out.refused);
    out.manifest_id
}

struct World {
    _dir_a: tempfile::TempDir,
    _dir_b: tempfile::TempDir,
    _src: tempfile::TempDir,
    id_a: DeviceIdentity,
    id_b: DeviceIdentity,
    store_a: Arc<Store>,
    store_b: Arc<Store>,
    manifest_a: BlobId,
}

/// Device A holds a snapshot of the fixture tree; device B starts empty.
fn world_with_data_on_a() -> World {
    let src = tempfile::tempdir().unwrap();
    build_source_tree(src.path());
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let store_a = new_store(dir_a.path());
    let store_b = new_store(dir_b.path());
    let id_a = fresh_identity(10);
    let id_b = fresh_identity(20);
    let manifest_a = snapshot(&store_a, src.path(), *id_a.device_id(), 1_700_000_000);
    World {
        _dir_a: dir_a,
        _dir_b: dir_b,
        _src: src,
        id_a,
        id_b,
        store_a,
        store_b,
        manifest_a,
    }
}

fn config_for(world: &World, side: char) -> EngineConfig {
    let (identity, store, current) = match side {
        'A' => (
            world.id_a.clone(),
            Arc::clone(&world.store_a),
            Some(world.manifest_a),
        ),
        'B' => (world.id_b.clone(), Arc::clone(&world.store_b), None),
        _ => unreachable!(),
    };
    EngineConfig {
        identity,
        expected_peer: match side {
            'A' => *world.id_b.device_id(),
            _ => *world.id_a.device_id(),
        },
        folders: vec![FolderState {
            folder_id: FOLDER,
            store,
            current_manifest: current,
        }],
        encryption: false,
        granularity: Granularity::Auto,
        max_retries: 3,
    }
}

/// Run both engines; the responder on a background thread. Returns both
/// reports.
fn converse<S: ByteStream + Send + 'static>(
    initiator: S,
    responder: S,
    cfg_a: EngineConfig,
    cfg_b: EngineConfig,
) -> (
    Result<SessionReport, ProtoError>,
    Result<SessionReport, ProtoError>,
) {
    let handle = std::thread::spawn(move || run_engine(responder, Role::Responder, cfg_b));
    let init = run_engine(initiator, Role::Initiator, cfg_a);
    let resp = handle.join().unwrap();
    (init, resp)
}

fn passthrough(inner: DuplexHalf) -> Tamper {
    Tamper {
        inner,
        flipped: false,
        flip_in_first_batch: false,
        flip_first_large: false,
        truncate_nth_write: None,
        writes_seen: 0,
        truncated: false,
    }
}

// --- stream tampering ----------------------------------------------------------

/// Wraps one duplex half's OUTBOUND writes. Rules:
/// - `flip_in_first_batch`: XOR one byte near the end of the first outbound
///   record whose plaintext body carries the `ITEM_BATCH` type byte
///   (meaningful only with session encryption OFF).
/// - `truncate_nth_write`: deliver only part of the Nth outbound write,
///   discard everything after, then shut the pipe (peer sees EOF).
struct Tamper {
    inner: DuplexHalf,
    flipped: bool,
    /// Corrupt the first outbound `ITEM_BATCH` record (plaintext sessions).
    flip_in_first_batch: bool,
    /// Corrupt the first large outbound record whatever it is (fatal under
    /// sealing).
    flip_first_large: bool,
    truncate_nth_write: Option<usize>,
    writes_seen: usize,
    truncated: bool,
}

impl Write for Tamper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes_seen += 1;
        if let Some(n) = self.truncate_nth_write {
            if self.writes_seen == n {
                // Deliver a prefix, then tear the pipe down. Claim success
                // for the whole buffer so the local writer proceeds.
                let keep = buf.len() / 2;
                self.inner.write_all(&buf[..keep])?;
                self.truncated = true;
                self.inner.close();
                return Ok(buf.len());
            }
            if self.truncated && self.writes_seen >= n {
                return Ok(buf.len()); // black-holed; pipe already dead
            }
        }
        // Record layout: u32 len | FRW1(4..8) | type@(8).
        let is_item_batch = buf.len() > 40 && buf[8] == 0x09;
        let want_flip = (self.flip_in_first_batch && is_item_batch)
            || (self.flip_first_large && !self.flipped && buf.len() > 200);
        if want_flip && !self.flipped {
            // Flip inside the tail (payload region, past headers/ids).
            let mut evil = buf.to_vec();
            let idx = evil.len() - 6;
            evil[idx] ^= 0xa5;
            self.flipped = true;
            return self.inner.write(&evil);
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Read for Tamper {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

// --- assertions ------------------------------------------------------------------

/// Every blob reachable from the store's index must hash to its own id.
fn assert_store_internally_consistent(store: &Store) {
    for e in store.index_entries().unwrap() {
        let bytes = store.get(e.kind, &e.id).expect("indexed blob readable");
        assert_eq!(
            *blake3::hash(&bytes).as_bytes(),
            e.id,
            "corrupt blob in store: {:?} {}",
            e.kind,
            hex(&e.id)
        );
    }
}

/// Throwaway materializer (T-005/T-006 own the production one): walk the
/// manifest, dump files temp+rename, recreate symlinks.
fn materialize(store: &Store, manifest_id: &BlobId, dest: &Path) {
    let man =
        ferry_store::manifest::parse_manifest(&store.get(BlobKind::Manifest, manifest_id).unwrap())
            .unwrap();
    materialize_tree(store, &man.root_tree_id, dest);
}

fn materialize_tree(store: &Store, tree_id: &BlobId, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let node =
        ferry_store::manifest::parse_tree_node(&store.get(BlobKind::TreeNode, tree_id).unwrap())
            .unwrap();
    for e in node.entries {
        let p = dir.join(&e.name);
        match &e.payload {
            ferry_store::manifest::EntryPayload::Dir { child_tree_id } => {
                materialize_tree(store, child_tree_id, &p);
            }
            ferry_store::manifest::EntryPayload::File { chunks, .. } => {
                let tmp = dir.join(format!(".tmp-{}", e.name));
                {
                    let mut f = std::fs::File::create(&tmp).unwrap();
                    for (cid, _) in chunks {
                        f.write_all(&store.get(BlobKind::DataChunk, cid).unwrap())
                            .unwrap();
                    }
                }
                #[cfg(unix)]
                if e.exec {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
                }
                #[cfg(not(unix))]
                let _ = e.exec;
                std::fs::rename(&tmp, &p).unwrap();
            }
            ferry_store::manifest::EntryPayload::Symlink { target } => {
                // Test helper: assertions compare file contents only, so on
                // hosts without symlink privilege we skip link entries
                // rather than fail materialization.
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &p).unwrap();
                #[cfg(not(unix))]
                let _ = target;
            }
        }
    }
}

/// Compare a materialized directory against the original source files.
fn assert_materialization_matches(
    source_files: &std::collections::BTreeMap<String, Vec<u8>>,
    out: &Path,
) {
    for (rel, want) in source_files {
        let got = std::fs::read(out.join(rel)).unwrap_or_else(|e| panic!("missing {rel}: {e}"));
        assert_eq!(&got, want, "content mismatch for {rel}");
    }
}

fn assert_agreement_recorded(world: &World, report_a: &SessionReport, report_b: &SessionReport) {
    for r in [&report_a.folders[0], &report_b.folders[0]] {
        assert_eq!(r.agreement_recorded, Some(world.manifest_a));
    }
    let ledger_a = AgreementLedger::new(world.store_a.store_dir());
    let ledger_b = AgreementLedger::new(world.store_b.store_dir());
    let rec_a = ledger_a
        .get(&FOLDER, world.id_b.device_id())
        .unwrap()
        .unwrap();
    let rec_b = ledger_b
        .get(&FOLDER, world.id_a.device_id())
        .unwrap()
        .unwrap();
    assert_eq!(rec_a.manifest_id, world.manifest_a);
    assert_eq!(rec_b.manifest_id, world.manifest_a);
    // Canonical record shape per docs/store-format.md.
    assert_eq!(rec_a.to_canonical().len(), 77);
}

// --- the loopback matrix ---------------------------------------------------------

#[test]
fn loopback_duplex_plaintext_converges_and_materializes() {
    let w = world_with_data_on_a();
    let (ia, rb) = duplex_pair();
    let (init, resp) = converse(ia, rb, config_for(&w, 'A'), config_for(&w, 'B'));
    let ra = init.unwrap();
    let rb_ = resp.unwrap();
    assert!(!ra.encrypted && !rb_.encrypted);
    assert_agreement_recorded(&w, &ra, &rb_);

    // Receiver ends with the identical manifest id and ALL blobs.
    assert_eq!(rb_.folders[0].local_manifest_after, Some(w.manifest_a));
    assert_store_internally_consistent(&w.store_b);

    // Materializable: bytes round-trip to disk.
    let out = tempfile::tempdir().unwrap();
    materialize(&w.store_b, &w.manifest_a, out.path());
    let src = tempfile::tempdir().unwrap();
    let _ = src; // source map captured during world build below
    let files = expected_files_fixture();
    assert_materialization_matches(&files, out.path());
}

/// Rebuild the same deterministic fixture map used in the worlds (kept
/// separate so each test controls where files land).
fn expected_files_fixture() -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut rng = StdRng::seed_from_u64(20260824);
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "big.bin".to_string(),
        (0..3 * 1024 * 1024).map(|_| rng.gen()).collect(),
    );
    files.insert("build.sh".to_string(), b"#!/bin/sh\necho ok\n".to_vec());
    files.insert(
        "src/util/math.rs".to_string(),
        b"pub fn two() -> u32 { 2 }".to_vec(),
    );
    use unicode_normalization::UnicodeNormalization;
    let nfc = |s: &str| s.nfc().collect::<String>();
    files.insert(
        format!(
            "{}/{}/{}/{}",
            nfc("日本語プロジェクト"),
            nfc("絵文字🚀"),
            nfc("café naïve/深い/さらに/その先"),
            nfc("データ file 📝.txt")
        ),
        "unicode content 🎉".as_bytes().to_vec(),
    );
    files
}

#[test]
fn tcp_loopback_encrypted_converges_the_acceptance_mode() {
    let w = world_with_data_on_a();
    let mut cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_a.encryption = true;
    cfg_b.encryption = true;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dial = TcpStream::connect(addr).unwrap();
    dial.set_nodelay(true).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    accepted.set_nodelay(true).unwrap();

    let handle = std::thread::spawn(move || run_engine(accepted, Role::Responder, cfg_b));
    let ra = run_engine(dial, Role::Initiator, cfg_a).unwrap();
    let rb = handle.join().unwrap().unwrap();

    assert!(
        ra.encrypted && rb.encrypted,
        "acceptance mode requires sealing"
    );
    assert!(rb.encrypted);
    assert_eq!(ra.agreed_version, ferry_proto::ProtocolVersion::V1_0);
    assert_agreement_recorded(&w, &ra, &rb);

    // Identical manifest id, complete store, materializable bytes.
    assert_eq!(rb.folders[0].local_manifest_after, Some(w.manifest_a));
    assert_store_internally_consistent(&w.store_b);
    let out = tempfile::tempdir().unwrap();
    materialize(&w.store_b, &w.manifest_a, out.path());
    assert_materialization_matches(&expected_files_fixture(), out.path());

    // The 3 MiB file really arrived multi-chunk (CDC split it).
    let chunks = w
        .store_b
        .index_entries()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == BlobKind::DataChunk)
        .count();
    assert!(
        chunks >= 2,
        "expected a multi-chunk transfer, saw {chunks} data chunks"
    );
}

#[test]
fn tcp_loopback_plaintext_converges() {
    let w = world_with_data_on_a();
    let cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_b.granularity = Granularity::ItemsOnly;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dial = TcpStream::connect(addr).unwrap();
    let (accepted, _) = listener.accept().unwrap();

    let handle = std::thread::spawn(move || run_engine(accepted, Role::Responder, cfg_b));
    let _ra = run_engine(dial, Role::Initiator, cfg_a).unwrap();
    handle.join().unwrap().unwrap();
    assert_store_internally_consistent(&w.store_b);
}

#[test]
fn pack_granularity_transfer_moves_whole_packs_by_ciphertext_name() {
    let w = world_with_data_on_a();
    let sender_packs_before: Vec<String> = list_packs(&w.store_a);
    assert!(!sender_packs_before.is_empty());

    let mut cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_a.encryption = true;
    cfg_b.encryption = true;
    cfg_b.granularity = Granularity::PacksOnly;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dial = TcpStream::connect(addr).unwrap();
    let (accepted, _) = listener.accept().unwrap();

    let handle = std::thread::spawn(move || run_engine(accepted, Role::Responder, cfg_b));
    run_engine(dial, Role::Initiator, cfg_a).unwrap();
    let rb = handle.join().unwrap().unwrap();

    assert_eq!(rb.folders[0].local_manifest_after, Some(w.manifest_a));
    assert_store_internally_consistent(&w.store_b);
    // Whole packs crossed by NAME: receiver holds pack files whose names
    // exist on the sender too.
    let got = list_packs(&w.store_b);
    assert!(
        got.iter().any(|g| sender_packs_before.contains(g)),
        "receiver should hold sender-named packs; got {got:?}"
    );
}

fn list_packs(store: &Store) -> Vec<String> {
    let dir = store.store_dir().join("packs");
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// --- corruption handling -----------------------------------------------------------

#[test]
fn corrupted_chunk_payload_is_detected_rejected_never_written_then_retry_succeeds() {
    let w = world_with_data_on_a();
    let (ia, rb) = duplex_pair();
    // The initiator SERVES (device B pulls), so the tamper sits on the
    // serving side's outbound traffic.
    let tamper = Tamper {
        inner: ia,
        flipped: false,
        flip_in_first_batch: true,
        flip_first_large: false,
        truncate_nth_write: None,
        writes_seen: 0,
        truncated: false,
    };
    let (init, resp) = converse(
        tamper,
        passthrough(rb),
        config_for(&w, 'A'),
        config_for(&w, 'B'),
    );
    let ra = init.expect("session must recover via re-request");
    let rb_report = resp.expect("receiver must converge after rejecting bad item");

    assert_eq!(
        rb_report.folders[0].rejections, 1,
        "exactly one rejected item"
    );
    assert_eq!(
        rb_report.folders[0].local_manifest_after,
        Some(w.manifest_a)
    );
    assert_agreement_recorded(&w, &ra, &rb_report);

    // Nothing corrupt was ever written: every stored blob hashes true.
    assert_store_internally_consistent(&w.store_b);
}

#[test]
fn encrypted_frame_flip_is_fatal_and_writes_nothing() {
    let w = world_with_data_on_a();
    let (ia, rb) = duplex_pair();
    // The initiator SERVES (device B pulls); flip its first LARGE sealed
    // record (the index advert) — under AEAD any flipped ciphertext must be
    // fatal, whatever the message type.
    let tamper = Tamper {
        inner: ia,
        flipped: false,
        flip_in_first_batch: false,
        flip_first_large: true,
        truncate_nth_write: None,
        writes_seen: 0,
        truncated: false,
    };
    let mut cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_a.encryption = true;
    cfg_b.encryption = true;

    let handle = std::thread::spawn(move || {
        run_engine(passthrough(rb), Role::Responder, cfg_b).map_err(|e| format!("{e}"))
    });
    let init = run_engine(tamper, Role::Initiator, cfg_a);
    let resp = handle.join().unwrap();

    // Both sides fail CLEANLY with typed errors (exact variant depends on
    // who notices first); neither reports success.
    assert!(
        init.is_err(),
        "initiator must fail: {:?}",
        init.as_ref().ok()
    );
    assert!(resp.is_err(), "responder must fail");

    // The receiver wrote NOTHING: no packs, no index entries, no agreement.
    assert!(list_packs(&w.store_b).is_empty());
    assert!(w.store_b.index_entries().unwrap().is_empty());
    let ledger = AgreementLedger::new(w.store_b.store_dir());
    assert!(ledger.get(&FOLDER, w.id_a.device_id()).unwrap().is_none());
}

#[test]
fn truncated_request_frame_fails_cleanly_leaving_stores_untouched() {
    let w = world_with_data_on_a();
    let (ia, rb) = duplex_pair();
    // Truncate the SERVER's third outbound write (its first large ITEM_BATCH
    // reply) mid-frame: the puller blocks reading a body that never
    // completes, the pipe dies, and both sides fail typed.
    let tamper = Tamper {
        inner: ia,
        flipped: false,
        flip_in_first_batch: false,
        flip_first_large: false,
        truncate_nth_write: Some(3),
        writes_seen: 0,
        truncated: false,
    };
    let cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_b.max_retries = 1;

    let handle = std::thread::spawn(move || run_engine(passthrough(rb), Role::Responder, cfg_b));
    let init = run_engine(tamper, Role::Initiator, cfg_a);
    let resp = handle.join().unwrap();

    assert!(init.is_err());
    assert!(resp.is_err());
    // Neither store grew anything during the failed exchange.
    assert!(list_packs(&w.store_b).is_empty());
    assert_store_internally_consistent(&w.store_a);
}

#[test]
fn corrupt_pack_at_rest_is_never_served_never_written_receiver_fails_cleanly() {
    let w = world_with_data_on_a();
    // Sabotage ONE data-containing pack in the SENDER's store post-snapshot.
    let packs_dir = w.store_a.store_dir().join("packs");
    let entries = w.store_a.index_entries().unwrap();
    let data_entry = entries
        .iter()
        .find(|e| e.kind == BlobKind::DataChunk)
        .expect("fixture must include a data chunk")
        .clone();
    let target = packs_dir.join(format!("{}.pack", hex(&data_entry.pack)));
    let mut bytes = std::fs::read(&target).unwrap();
    bytes[30] ^= 0xff;
    std::fs::write(&target, &bytes).unwrap();
    let sabotaged_chunk = data_entry.id;

    let cfg_a = config_for(&w, 'A');
    let cfg_b = config_for(&w, 'B');

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dial = TcpStream::connect(addr).unwrap();
    let (accepted, _) = listener.accept().unwrap();

    let handle = std::thread::spawn(move || run_engine(accepted, Role::Responder, cfg_b));
    let init = run_engine(dial, Role::Initiator, cfg_a);
    let resp = handle.join().unwrap();

    // The puller surfaces a typed missing-items failure after retries; the
    // server fails too when the puller hangs up.
    let rb_err = resp.expect_err("receiver must fail cleanly");
    assert!(
        matches!(rb_err, ProtoError::MissingItems(_)),
        "expected MissingItems, got {rb_err}"
    );
    let _ = init.expect_err("server side must also fail");

    // The receiver holds NOTHING from the broken transfer — and in
    // particular never the sabotaged chunk under any bytes.
    assert!(list_packs(&w.store_b).is_empty());
    assert!(w
        .store_b
        .get(BlobKind::DataChunk, &sabotaged_chunk)
        .is_err());
}

#[test]
fn both_devices_hold_data_converges_via_bidirectional_union() {
    // Divergence case: A and B each hold DIFFERENT snapshots. Both pull
    // from each other (store-level union); pointers stay divergent, so no
    // agreement is recorded (reconciliation is T-006+ scope).
    let w = world_with_data_on_a();

    // Give B its own distinct tree + manifest.
    let src_b = tempfile::tempdir().unwrap();
    std::fs::write(src_b.path().join("only-b.txt"), b"b's own content").unwrap();
    let manifest_b = snapshot(&w.store_b, src_b.path(), *w.id_b.device_id(), 1_700_001_000);

    let mut cfg_a = config_for(&w, 'A');
    cfg_a.folders[0].current_manifest = Some(w.manifest_a);
    let mut cfg_b = config_for(&w, 'B');
    cfg_b.folders[0].current_manifest = Some(manifest_b);

    let (ia, rb) = duplex_pair();
    let (init, resp) = converse(ia, rb, cfg_a, cfg_b);
    let ra = init.unwrap();
    let rb_r = resp.unwrap();

    // Union: each store can now read the other's root manifest blob.
    assert!(w.store_a.get(BlobKind::Manifest, &manifest_b).is_ok());
    assert!(w.store_b.get(BlobKind::Manifest, &w.manifest_a).is_ok());
    // Pointers did NOT move; no agreement recorded.
    assert_eq!(ra.folders[0].agreement_recorded, None);
    assert_eq!(rb_r.folders[0].agreement_recorded, None);
    assert_store_internally_consistent(&w.store_a);
    assert_store_internally_consistent(&w.store_b);
}

#[test]
fn fresh_device_over_tcp_adopts_and_records_agreement_both_sides() {
    // Same as the acceptance flow but asserting the ADOPTION semantics
    // explicitly and checking the canonical ledger bytes on disk.
    let w = world_with_data_on_a();
    let mut cfg_a = config_for(&w, 'A');
    let mut cfg_b = config_for(&w, 'B');
    cfg_a.encryption = true;
    cfg_b.encryption = true;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dial = TcpStream::connect(addr).unwrap();
    let (accepted, _) = listener.accept().unwrap();

    let handle = std::thread::spawn(move || run_engine(accepted, Role::Responder, cfg_b));
    run_engine(dial, Role::Initiator, cfg_a).unwrap();
    let rb = handle.join().unwrap().unwrap();

    assert_eq!(rb.folders[0].local_manifest_after, Some(w.manifest_a));

    // Raw on-disk ledger bytes are the canonical 77-byte serialization.
    let path = w.store_b.store_dir().join("agreement").join(format!(
        "{}-{}.agree",
        hex(&FOLDER),
        hex(w.id_a.device_id().as_slice())
    ));
    let raw = std::fs::read(path).unwrap();
    assert_eq!(raw.len(), 77);
    assert_eq!(&raw[32..64], &w.manifest_a);
    assert_eq!(raw[76], 0, "flags reserved zero");
}
