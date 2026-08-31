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
use ferry_sync::{run_v1_session, CurrentState, Established, ExchangeHost, DEFAULT_FOLDER_ID};

const POLY_SEED: u64 = 20260824;

fn poly() -> ferry_store::chunker::ValidatedPoly {
    ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(POLY_SEED))
}

fn open_store(dir: &Path) -> Arc<Store> {
    let identity = DeviceIdentity::from_secret_bytes(&[0xA1u8; 32]);
    ferry_folder::open_or_create_test_store(dir, &identity).unwrap()
}

fn pair_stores(
    dir_a: &Path,
    id_a: &DeviceIdentity,
    dir_b: &Path,
    id_b: &DeviceIdentity,
) -> (Arc<Store>, Arc<Store>) {
    let p = poly().get();
    let (store_a, fmk) =
        ferry_folder::folder::create_folder(dir_a, id_a, DEFAULT_FOLDER_ID, p).unwrap();
    ferry_folder::folder::save_settings(
        dir_a,
        &ferry_folder::folder::Settings {
            format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&DEFAULT_FOLDER_ID),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    store_a.flush().unwrap();
    store_a.write_index_snapshot().unwrap();

    let store_b =
        ferry_folder::folder::adopt_folder(dir_b, id_b, DEFAULT_FOLDER_ID, &fmk, p).unwrap();
    ferry_folder::folder::save_settings(
        dir_b,
        &ferry_folder::folder::Settings {
            format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&DEFAULT_FOLDER_ID),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    store_b.flush().unwrap();
    store_b.write_index_snapshot().unwrap();

    (Arc::new(store_a), Arc::new(store_b))
}

fn ident(tag: &str) -> DeviceIdentity {
    let mut sk = [0u8; 32];
    for (i, b) in sk.iter_mut().enumerate() {
        *b = blake3::hash(format!("interop/{tag}:{i}").as_bytes()).as_bytes()[i % 32]
            ^ (i as u8).wrapping_mul(31);
    }
    DeviceIdentity::from_secret_bytes(&sk)
}

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

fn snapshot_empty(store: &Store, tree: &Path, who: &DeviceIdentity) -> RootManifest {
    use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
    fs::create_dir_all(tree).unwrap();
    let identity = SnapshotIdentity {
        folder_id: DEFAULT_FOLDER_ID,
        device_id: *who.device_id(),
        parent_manifest_id: [0; 32],
        created_sec: 1_700_000_001,
        created_nsec: 0,
    };
    snapshot_dir(store, poly(), tree, &identity)
        .unwrap()
        .manifest
}

fn manifest_id_of(m: &RootManifest) -> [u8; 32] {
    *blake3::hash(&ferry_store::manifest::serialize_manifest(m)).as_bytes()
}

struct TestHost {
    tree_root: PathBuf,
    adopted: Mutex<Vec<[u8; 32]>>,
    agreed: Mutex<Option<[u8; 32]>>,

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
    let (store_ref, store_my) = pair_stores(
        &dir.path().join("ref"),
        &id_ref,
        &dir.path().join("my"),
        &id_my,
    );

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

    let report = server.join().unwrap().expect("reference engine ok");
    assert!(report.encrypted);
    assert_eq!(
        report.folders[0].agreement_recorded,
        Some(ref_manifest_id),
        "reference recorded agreement on its own manifest"
    );

    assert_eq!(*host.agreed.lock().unwrap(), Some(ref_manifest_id));
    assert_eq!(
        host.adopted.lock().unwrap().last(),
        Some(&ref_manifest.root_tree_id)
    );

    assert!(trees_identical(&ref_tree, &my_tree), "working trees match");

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
    let (store_ref, store_my) = pair_stores(
        &dir.path().join("ref"),
        &id_ref,
        &dir.path().join("my"),
        &id_my,
    );

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
        tree_root: my_tree.clone(),
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

    assert_eq!(report.folders[0].agreement_recorded, Some(my_manifest_id));
    assert_eq!(report.folders[0].local_manifest_after, Some(my_manifest_id));
    assert_eq!(*host.agreed.lock().unwrap(), Some(my_manifest_id));

    for kind in [
        ferry_store::BlobKind::Manifest,
        ferry_store::BlobKind::TreeNode,
    ] {
        let _ = kind;
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
        n: 3,
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

    let payload = ferry_proto::codec::FolderOffer {
        folder_id: DEFAULT_FOLDER_ID,
        manifest_id: [0; 32],
        reserved: 0,
    }
    .encode();
    est.io
        .send_frame(ferry_proto::codec::MSG_FOLDER_OFFER, payload)
        .unwrap();

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
                Ok(_) => Ok(()),
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

    est.io.send_frame(0x7F, vec![1, 2, 3]).unwrap();

    let Err(got) = hb.join().unwrap() else {
        panic!("receiver accepted an unknown message type")
    };
    assert!(
        matches!(got, ProtoError::UnknownMessage { msg_type: 0x7F }),
        "{got}"
    );
}
