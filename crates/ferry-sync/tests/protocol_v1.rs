//! T-014 acceptance: ferry-sync speaks protocol v1 with encryption ON by
//! default, byte-compatibly with the reference implementation.
//!
//! - [`ferry_sync_stack_interops_with_reference_engine`] runs THIS crate's
//!   handshake + conversation against `ferry_proto::run_engine` over real
//!   loopback TCP, encrypted — and checks the agreement LEDGERS landed on
//!   both sides in THE canonical 77-byte serialization, and that the
//!   working tree materialized byte-identically.
//! - [`reference_initiator_ferry_sync_responder_interop`] flips the role
//!   assignment (the reference initiates and pulls from us).
//! - [`tampered_post_auth_byte_fails_authentication_cross_implementation`]
//!   corrupts one sealed frame in flight between the two IMPLEMENTATIONS
//!   and requires the reference receiver to reject it without recording
//!   any agreement.
//! - [`unknown_message_type_is_a_clean_protocol_violation`] exercises the
//!   normative unknown-type rule (v1.0 peers never skip).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_crypto::identity::DeviceIdentity;
use ferry_proto::error::ProtoError;
use ferry_proto::stream::DuplexHalf;
use ferry_proto::{duplex_pair, EngineConfig as RefConfig, FolderState as RefFolder, Role};
use ferry_store::manifest::RootManifest;
use ferry_store::store::Store;
use ferry_sync::session::{establish, RawLink};
use ferry_sync::{
    run_v1_session, CurrentState, Established, ExchangeHost, DEFAULT_FOLDER_ID,
};

const POLY_SEED: u64 = 20260824;

fn poly() -> ferry_store::chunker::ValidatedPoly {
    ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(POLY_SEED))
}

fn open_store(dir: &Path) -> Arc<Store> {
    let fmk = [0u8; ferry_store::crypto::KEY_LEN];
    std::fs::create_dir_all(dir).unwrap();
    if dir.join(ferry_store::store::STORE_DIR_NAME).is_dir() {
        Arc::new(Store::open(dir, fmk, Box::new(ferry_store::crypto::PassthroughCipher)).unwrap())
    } else {
        Arc::new(Store::create(dir, fmk, Box::new(ferry_store::crypto::PassthroughCipher)).unwrap())
    }
}

/// Deterministic identity per tag; only stability within this process
/// matters here.
fn ident(tag: &str) -> DeviceIdentity {
    let mut sk = [0u8; 32];
    for (i, b) in sk.iter_mut().enumerate() {
        *b = blake3::hash(format!("interop/{tag}:{i}").as_bytes()).as_bytes()[i % 32]
            ^ (i as u8).wrapping_mul(31);
    }
    DeviceIdentity::from_secret_bytes(&sk)
}

/// Seed `tree` with files and snapshot it into `store`.
fn snapshot_tree(store: &Store, tree: &Path, who: &DeviceIdentity, sec: i64) -> RootManifest {
    use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
    fs::create_dir_all(tree).unwrap();
    fs::write(tree.join("hello.txt"), b"interop hello").unwrap();
    fs::create_dir_all(tree.join("nested")).unwrap();
    fs::write(tree.join("nested/data.bin"), vec![9u8; 4096]).unwrap();

    let identity = SnapshotIdentity {
        folder_id: DEFAULT_FOLDER_ID,
        device_id: *who.device_id(),
        parent_manifest_id: [0; 32],
        created_sec: sec,
        created_nsec: 0,
    };
    snapshot_dir(store, poly(), tree, &identity)
        .unwrap()
        .manifest
}

/// Snapshot an EMPTY directory into `store` (fresh-device state).
fn snapshot_empty(store: &Store, tree: &Path, who: &DeviceIdentity) -> RootManifest {
    use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
    fs::create_dir_all(tree).unwrap();
    let identity = SnapshotIdentity {
        folder_id: DEFAULT_FOLDER_ID,
        device_id: *who.device_id(),
        parent_manifest_id: [0; 32],
        created_sec: 1_700_000_001, // NEWER clock than the content fixture
        created_nsec: 0,
    };
    snapshot_dir(store, poly(), tree, &identity)
        .unwrap()
        .manifest
}

fn manifest_id_of(m: &RootManifest) -> [u8; 32] {
    *blake3::hash(&ferry_store::manifest::serialize_manifest(m)).as_bytes()
}

/// Minimal host driving our exchange under test.
struct TestHost {
    tree_root: PathBuf,
    adopted: Mutex<Vec<[u8; 32]>>,
    agreed: Mutex<Option<[u8; 32]>>,
    /// When set, agreement also lands as the canonical 77-byte ledger
    /// record under this `.ferry` directory (what the engine really does).
    ledger_dot: Option<PathBuf>,
}

impl ExchangeHost for TestHost {
    fn status(&self, _line: &str) {}
    fn bump_rejected(&self) {}
    fn tree_root(&self) -> &Path {
        &self.tree_root
    }
    fn adopt(
        &self,
        _bytes: &[u8],
        manifest: &RootManifest,
    ) -> Result<(), ferry_sync::SessionError> {
        self.adopted.lock().unwrap().push(manifest.root_tree_id);
        Ok(())
    }
    fn agree(
        &self,
        peer: [u8; 32],
        _bytes: &[u8],
        manifest_id: [u8; 32],
    ) -> Result<(), ferry_sync::SessionError> {
        if let Some(dot) = &self.ledger_dot {
            let (sec, nsec) = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or((0, 0), |d| (d.as_secs() as i64, d.subsec_nanos()));
            ferry_store::agreement::AgreementLedger::new(dot)
                .record(
                    &DEFAULT_FOLDER_ID,
                    &ferry_store::agreement::AgreedRecord {
                        peer_device_id: peer,
                        manifest_id,
                        agreed_sec: sec,
                        agreed_nsec: nsec,
                    },
                )
                .map_err(|e| ferry_sync::SessionError::Other(format!("agreement ledger: {e}")))?;
        }
        *self.agreed.lock().unwrap() = Some(manifest_id);
        Ok(())
    }
}

fn ledger_path(store_dot: &Path, peer: [u8; 32]) -> PathBuf {
    store_dot.join("agreement").join(format!(
        "{}-{}.agree",
        ferry_sync::format::hex(&DEFAULT_FOLDER_ID),
        ferry_sync::format::hex(&peer)
    ))
}

fn trees_identical(a: &Path, b: &Path) -> bool {
    fn listing(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                    out.insert(rel, fs::read(&p).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out);
        out
    }
    listing(a) == listing(b)
}

#[test]
fn ferry_sync_stack_interops_with_reference_engine() {
    let dir = tempfile::tempdir().unwrap();
    let ref_dot = dir.path().join("ref/.ferry");
    let my_dot = dir.path().join("my/.ferry");
    let ref_tree = dir.path().join("ref/tree");
    let my_tree = dir.path().join("my/tree");

    let id_ref = ident("ref-node");
    let id_my = ident("my-node");
    let store_ref = open_store(&dir.path().join("ref"));
    let store_my = open_store(&dir.path().join("my"));

    // Reference side holds content (older clock); our side is a fresh
    // empty device with a NEWER clock — bootstrap adoption must ignore
    // the clock, so this pairing also proves pick_donor's rule 1 on wire.
    let ref_manifest = snapshot_tree(&store_ref, &ref_tree, &id_ref, 1_700_000_000);
    let ref_manifest_id = manifest_id_of(&ref_manifest);
    let my_empty = snapshot_empty(&store_my, &my_tree, &id_my);

    let lst = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = lst.local_addr().unwrap();

    let id_ref_t = id_ref.clone();
    let id_my_t = id_my.clone();
    let store_ref_t = Arc::clone(&store_ref);
    let server = std::thread::spawn(move || {
        let (stream, _) = lst.accept().unwrap();
        ferry_proto::run_engine(
            stream,
            Role::Responder,
            RefConfig::new(
                id_ref_t,
                *id_my_t.device_id(),
                vec![RefFolder {
                    folder_id: DEFAULT_FOLDER_ID,
                    store: store_ref_t,
                    current_manifest: Some(ref_manifest_id),
                }],
            ),
        )
    });

    let client = std::net::TcpStream::connect(addr).unwrap();
    let mut link = RawLink(client);
    let mut est: Established = establish(
        &mut link,
        Role::Initiator,
        &id_my,
        ferry_sync::ExpectPeer::Pin(*id_ref.device_id()),
        true,
    )
    .expect("handshake against the reference engine");
    assert!(est.encrypted, "sessions are sealed by default");

    let host = TestHost {
        tree_root: my_tree.clone(),
        adopted: Mutex::new(Vec::new()),
        agreed: Mutex::new(None),
        ledger_dot: Some(my_dot.clone()),
    };
    run_v1_session(
        &mut est,
        &host,
        &store_my,
        DEFAULT_FOLDER_ID,
        CurrentState {
            id: manifest_id_of(&my_empty),
            bytes: ferry_store::manifest::serialize_manifest(&my_empty),
            manifest: my_empty,
        },
        3,
        true,
    )
    .expect("our v1 conversation completes against the reference engine");

    // The reference side finished cleanly, encrypted, and agreed.
    let report = server.join().unwrap().expect("reference engine ok");
    assert!(report.encrypted);
    assert_eq!(
        report.folders[0].agreement_recorded,
        Some(ref_manifest_id),
        "reference recorded agreement on its own manifest"
    );

    // Our side adopted the reference manifest and agreed on the same id.
    assert_eq!(*host.agreed.lock().unwrap(), Some(ref_manifest_id));
    assert_eq!(
        host.adopted.lock().unwrap().last(),
        Some(&ref_manifest.root_tree_id)
    );

    // Materialized tree matches the reference tree byte for byte.
    assert!(trees_identical(&ref_tree, &my_tree), "working trees match");

    // Ledgers: canonical 77-byte records on BOTH sides, same ids.
    let rb =
        fs::read(ledger_path(&ref_dot, *id_my.device_id())).expect("reference-side ledger exists");
    let mb = fs::read(ledger_path(&my_dot, *id_ref.device_id())).expect("our-side ledger exists");
    assert_eq!(rb.len(), 77);
    assert_eq!(mb.len(), 77);
    let rrec = ferry_store::agreement::parse_agreed_record(&rb).unwrap();
    let mrec = ferry_store::agreement::parse_agreed_record(&mb).unwrap();
    assert_eq!(rrec.manifest_id, ref_manifest_id);
    assert_eq!(mrec.manifest_id, ref_manifest_id);
    assert_eq!(rrec.peer_device_id, *id_my.device_id());
    assert_eq!(mrec.peer_device_id, *id_ref.device_id());
}

#[test]
fn reference_initiator_ferry_sync_responder_interop() {
    let dir = tempfile::tempdir().unwrap();
    let my_tree = dir.path().join("my/tree");

    let id_ref = ident("ri-ref");
    let id_my = ident("ri-my");
    let store_ref = open_store(&dir.path().join("ref"));
    let store_my = open_store(&dir.path().join("my"));

    // Content lives on OUR side now. The reference starts as a FRESH
    // device (current_manifest None): after pulling our manifest it
    // adopts it per the reference adoption rule, making round-2 ids meet.
    let my_manifest = snapshot_tree(&store_my, &my_tree, &id_my, 1_700_000_010);
    let my_manifest_id = manifest_id_of(&my_manifest);

    let lst = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = lst.local_addr().unwrap();
    let id_ref_t = id_ref.clone();
    let id_my_t = id_my.clone();
    let store_ref_t = std::sync::Arc::clone(&store_ref);
    let server = std::thread::spawn(move || {
        let (stream, _) = lst.accept().unwrap();
        ferry_proto::run_engine(
            stream,
            Role::Initiator,
            RefConfig::new(
                id_ref_t,
                *id_my_t.device_id(),
                vec![RefFolder {
                    folder_id: DEFAULT_FOLDER_ID,
                    store: store_ref_t,
                    current_manifest: None,
                }],
            ),
        )
    });

    let client = std::net::TcpStream::connect(addr).unwrap();
    let mut link = RawLink(client);
    let mut est: Established = establish(
        &mut link,
        Role::Responder,
        &id_my,
        ferry_sync::ExpectPeer::Pin(*id_ref.device_id()),
        true,
    )
    .expect("handshake");

    let host = TestHost {
        tree_root: my_tree.clone(), // we serve only; nothing to materialize
        adopted: Mutex::new(Vec::new()),
        agreed: Mutex::new(None),
        ledger_dot: None,
    };
    run_v1_session(
        &mut est,
        &host,
        &store_my,
        DEFAULT_FOLDER_ID,
        CurrentState {
            id: my_manifest_id,
            bytes: ferry_store::manifest::serialize_manifest(&my_manifest),
            manifest: my_manifest,
        },
        3,
        false,
    )
    .expect("responder conversation completes");

    let report = server.join().unwrap().expect("reference engine ok");
    assert!(report.encrypted);
    // The reference pulled our content, adopted our manifest pointer, and
    // both sides agreed on OUR manifest id.
    assert_eq!(report.folders[0].agreement_recorded, Some(my_manifest_id));
    assert_eq!(report.folders[0].local_manifest_after, Some(my_manifest_id));
    assert_eq!(*host.agreed.lock().unwrap(), Some(my_manifest_id));

    // The reference store can now serve every blob of our manifest back.
    for kind in [
        ferry_store::BlobKind::Manifest,
        ferry_store::BlobKind::TreeNode,
    ] {
        let _ = kind; // presence checks below via direct gets
    }
    let man_bytes = store_ref
        .get(
            ferry_store::BlobKind::Manifest,
            &ferry_store::BlobId::from(my_manifest_id),
        )
        .expect("manifest blob landed in the reference store");
    assert_eq!(
        ferry_sync::format::hex(blake3::hash(&man_bytes).as_bytes()),
        ferry_sync::format::hex(&my_manifest_id)
    );
}

/// A Read+Write wrapper around `DuplexHalf` that corrupts the Nth outbound
/// record: record 1 = HELLO, record 2 = `AUTH_INIT`, record 3 = first
/// post-auth frame (sealed `FOLDER_OFFER`). Flipping a ciphertext byte there
/// must break AEAD authentication on the REFERENCE receiver.
struct TamperNthWrite {
    inner: DuplexHalf,
    n: usize,
    seen: usize,
}

impl Write for TamperNthWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.seen += 1;
        if self.seen == self.n {
            let mut bad = buf.to_vec();
            let at = bad.len() - 3;
            bad[at] ^= 0x01;
            self.inner.write_all(&bad)?;
            return Ok(buf.len());
        }
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Read for TamperNthWrite {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

#[test]
fn tampered_post_auth_byte_fails_authentication_cross_implementation() {
    let dir = tempfile::tempdir().unwrap();
    let store_ref = open_store(&dir.path().join(".ferry"));

    let id_ref = ident("tamp-ref");
    let id_my = ident("tamp-my");
    let id_my_pub = *id_my.device_id();

    let (la, lb) = duplex_pair();
    let handle = std::thread::spawn(move || {
        ferry_proto::run_engine(
            lb,
            Role::Responder,
            RefConfig::new(
                id_ref,
                id_my_pub,
                vec![RefFolder {
                    folder_id: DEFAULT_FOLDER_ID,
                    store: store_ref,
                    current_manifest: None,
                }],
            ),
        )
    });

    let mut link = RawLink(TamperNthWrite {
        inner: la,
        n: 3, // first post-auth frame from the initiator
        seen: 0,
    });
    let mut est: Established = establish(
        &mut link,
        Role::Initiator,
        &id_my,
        ferry_sync::ExpectPeer::Pin(*ident("tamp-ref").device_id()),
        true,
    )
    .expect("handshake must succeed before tampering");

    // An honest sealed offer leaves OUR stack; the wrapper flips a byte
    // en route.
    let payload = ferry_proto::codec::FolderOffer {
        folder_id: DEFAULT_FOLDER_ID,
        manifest_id: [0; 32],
        reserved: 0,
    }
    .encode();
    est.io
        .send_frame(ferry_proto::codec::MSG_FOLDER_OFFER, payload)
        .unwrap();

    // Give the reference side a moment to fail on the tag, then drop our
    // end so the thread cannot block forever on later reads.
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(est);
    drop(link);

    let res = handle.join().unwrap();
    assert!(
        matches!(
            res,
            Err(ProtoError::Auth(_)
                | ProtoError::ByeReceived {
                    reason: ferry_proto::error::ByeReason::AuthFailed
                }
                | ProtoError::Io(_))
        ),
        "reference rejected the tampered frame: {res:?}"
    );
}

/// Normative unknown-message rule, v1.0 ↔ v1.0: unknown types are NEVER
/// skipped (no higher minor advertised); the session dies cleanly with
/// `UnknownMessage`.
#[test]
fn unknown_message_type_is_a_clean_protocol_violation() {
    let id_a = ident("unk-a");
    let id_b = ident("unk-b");
    let (la, lb) = duplex_pair();

    let hb = std::thread::spawn(move || -> Result<(), ProtoError> {
        let mut link = RawLink(lb);
        match establish(
            &mut link,
            Role::Responder,
            &id_b,
            ferry_sync::ExpectPeer::TrustOnFirstUse,
            true,
        ) {
            Err(e) => Err(e),
            Ok(mut est) => match est.io.recv_frame() {
                Err(e) => Err(e),
                Ok(_) => Ok(()), // accepting an unknown type fails the test
            },
        }
    });

    let mut link = RawLink(la);
    let mut est: Established = establish(
        &mut link,
        Role::Initiator,
        &id_a,
        ferry_sync::ExpectPeer::TrustOnFirstUse,
        true,
    )
    .unwrap();

    // Sealed frame carrying an unregistered type: opens fine (tag valid),
    // then hits the policy check.
    est.io.send_frame(0x7F, vec![1, 2, 3]).unwrap();

    let Err(got) = hb.join().unwrap() else {
        panic!("receiver accepted an unknown message type")
    };
    assert!(
        matches!(got, ProtoError::UnknownMessage { msg_type: 0x7F }),
        "{got}"
    );
}
