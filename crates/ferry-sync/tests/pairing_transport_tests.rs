use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::folder::create_folder;
use ferry_ipc::pairing::JoinPairingRequest;
use ferry_store::chunker::ValidatedPoly;
use rand::SeedableRng;

use ferry_sync::pairing_transport::{new_shared_rendezvous, PairingTransport};

fn poly_for_test(seed: u64) -> ValidatedPoly {
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    ValidatedPoly::generate(&mut rng)
}

fn temp_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (dir, home)
}

fn create_test_folder(home: &Path, identity: &DeviceIdentity, _poly: ValidatedPoly) -> (PathBuf, [u8; 16]) {
    create_test_folder_with_id(home, identity, rand::random())
}

fn create_test_folder_with_id(home: &Path, identity: &DeviceIdentity, folder_id: [u8; 16]) -> (PathBuf, [u8; 16]) {
    let folder_path = home.join(format!("folder-{}", &ferry_store::format::hex(&folder_id)[..8]));
    std::fs::create_dir_all(&folder_path).unwrap();
    let poly_u64: u64 = 0x1234567890abcdef;
    let (store, _fmk) = create_folder(&folder_path, identity, folder_id, poly_u64).unwrap();
    let settings = ferry_folder::folder::Settings {
        format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&folder_id),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    ferry_folder::folder::save_settings(&folder_path, &settings).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    (folder_path, folder_id)
}

#[test]
fn create_session_generates_6_char_code_and_join_wrong_code_fails() {
    let (_home_tmp, home) = temp_home();
    let id_a = DeviceIdentity::generate();
    let poly = poly_for_test(1);
    let (folder_path, folder_id) = create_test_folder(&home, &id_a, poly);
    let folder_id_hex = ferry_store::format::hex(&folder_id);
    let transport_a = PairingTransport::new(home.clone(), id_a.clone());
    transport_a.register_folder_path(folder_id_hex.clone(), folder_path.clone());

    let resp = transport_a.create_session(folder_id_hex.clone()).expect("create");
    assert_eq!(resp.code.len(), 6, "code is 6 chars");
    assert!(resp.code.chars().all(|c| c.is_ascii_alphanumeric() && !c.is_ascii_lowercase()));
    assert!(!resp.expires_at.is_empty());

    // No pair-offer files at $FERRY_HOME or folder.
    assert!(!home.join("pair-offer.ferry-pair").exists());
    assert!(!home.join(format!("pair-{}", resp.code)).exists());
    assert!(!folder_path.join(".ferry/pair-offer.ferry-pair").exists());
    assert!(!folder_path.join(".ferry/pair-offer").exists());

    // Join with wrong code -> pairing-not-found
    let transport_b = PairingTransport::with_shared(home.clone(), DeviceIdentity::generate(), transport_a.shared());
    let join_req = JoinPairingRequest::new("ZZZZZZ".to_string(), home.join("target-wrong"));
    let err = transport_b.join_session(join_req).unwrap_err();
    assert_eq!(err.code, "pairing-not-found");

    // Correct code succeeds over in-memory rendezvous
    let target = home.join("target-ok");
    std::fs::create_dir_all(&target).unwrap();
    let join_req = JoinPairingRequest::new(resp.code.clone(), target.clone());
    let result = transport_b.join_session(join_req).expect("join");
    assert_eq!(result.folder_id.to_ascii_lowercase(), folder_id_hex.to_ascii_lowercase());
    assert_eq!(result.folder_path, target);
    assert_eq!(result.status, "paired");
    // Ensure no pair-offer files were written at $FERRY_HOME after join either
    assert!(!home.join("pair-offer.ferry-pair").exists());
    assert!(!home.join(format!("pair-{}", resp.code)).exists());
    // Re-joining same code should fail (one-time)
    let target2 = home.join("target-reuse");
    std::fs::create_dir_all(&target2).unwrap();
    let err = transport_b
        .join_session(JoinPairingRequest::new(resp.code, target2))
        .unwrap_err();
    assert_eq!(err.code, "pairing-not-found");
}

#[test]
fn in_memory_e2e_handshake_persists_wrapped_fmk() {
    let (_home_tmp, home) = temp_home();
    let id_a = DeviceIdentity::generate();
    let id_b = DeviceIdentity::generate();
    let folder_id: [u8; 16] = [0x11; 16];
    let (folder_path_a, _) = create_test_folder_with_id(&home, &id_a, folder_id);
    let folder_id_hex = ferry_store::format::hex(&folder_id);
    let shared = new_shared_rendezvous();
    let transport_a = PairingTransport::with_shared(home.clone(), id_a.clone(), Arc::clone(&shared));
    transport_a.register_folder_path(folder_id_hex.clone(), folder_path_a.clone());
    let transport_b = PairingTransport::with_shared(home.clone(), id_b.clone(), Arc::clone(&shared));

    let resp = transport_a.create_session(folder_id_hex.clone()).unwrap();
    let target_b = home.join("folder-b-e2e");
    std::fs::create_dir_all(&target_b).unwrap();
    let result = transport_b
        .join_session(JoinPairingRequest::new(resp.code, target_b.clone()))
        .unwrap();
    assert_eq!(result.folder_id, folder_id_hex);

    // B's folder has CONFIG_HEAD with wrapped FMK and folder_id matches
    let config_path = target_b.join(".ferry/config");
    assert!(config_path.exists(), "CONFIG_HEAD should exist at {}", config_path.display());
    let bytes = std::fs::read(&config_path).unwrap();
    let head = ferry_crypto::config_head::parse_config_head(&bytes).unwrap();
    assert_eq!(head.folder_id, folder_id);
    assert!(head.entries.iter().any(|e| e.device_pub == *id_b.public()));
    assert!(head.entries.iter().any(|e| e.device_pub == *id_a.public()));
    // B can unwrap FMK
    let entry_b = head.entries.iter().find(|e| e.device_pub == *id_b.public()).unwrap();
    let fmk_b = ferry_crypto::folder_key::unwrap_folder_key(&entry_b.wrapped, &folder_id, &id_b).unwrap();
    // A's config also has B's entry (mutual)
    let config_a = std::fs::read(folder_path_a.join(".ferry/config")).unwrap();
    let head_a = ferry_crypto::config_head::parse_config_head(&config_a).unwrap();
    assert!(head_a.entries.iter().any(|e| e.device_pub == *id_b.public()));
    // FMKs are same
    let entry_a = head_a.entries.iter().find(|e| e.device_pub == *id_a.public()).unwrap();
    let fmk_a = ferry_crypto::folder_key::unwrap_folder_key(&entry_a.wrapped, &folder_id, &id_a).unwrap();
    assert_eq!(*fmk_a, *fmk_b);
}

#[test]
fn expiry_returns_pairing_expired() {
    let (_home_tmp, home) = temp_home();
    let id_a = DeviceIdentity::generate();
    let folder_id: [u8; 16] = [0x22; 16];
    let (folder_path, _) = create_test_folder_with_id(&home, &id_a, folder_id);
    let folder_id_hex = ferry_store::format::hex(&folder_id);
    let shared = new_shared_rendezvous();
    let transport_a = PairingTransport::with_shared(home.clone(), id_a.clone(), Arc::clone(&shared));
    transport_a.register_folder_path(folder_id_hex.clone(), folder_path);
    let transport_b = PairingTransport::with_shared(home.clone(), DeviceIdentity::generate(), Arc::clone(&shared));

    let resp = transport_a.create_session(folder_id_hex).unwrap();
    // Force expiry by mutating the stored session's expires_at to past
    {
        let mut m = shared.lock().unwrap();
        let key = resp.code.to_ascii_uppercase();
        if let Some(sess) = m.get_mut(&key) {
            sess.expires_at = SystemTime::now() - Duration::from_secs(10);
        }
    }
    let target = home.join("target-expiry");
    std::fs::create_dir_all(&target).unwrap();
    let err = transport_b
        .join_session(JoinPairingRequest::new(resp.code, target))
        .unwrap_err();
    assert_eq!(err.code, "pairing-expired");
}

#[test]
fn uibackend_fakebackend_create_and_join_roundtrip() {
    // Tests UiBackend seam: FakeBackend create + join via same instance (in-memory rendezvous)
    use ferry_ipc::backend::{FakeBackend, UiBackend};
    use ferry_ipc::pairing::{CreatePairingRequest, JoinPairingRequest};
    use std::path::PathBuf;

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let backend = FakeBackend::new();
        let resp = backend
            .create_pairing_session(CreatePairingRequest::new(
                "0123456789abcdef0123456789abcdef".to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.code.len(), 6);
        // Wrong code -> not found
        let err = backend
            .join_pairing_session(JoinPairingRequest::new(
                "WRONG1".to_string(),
                PathBuf::from("/tmp/target-wrong"),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.code, "pairing-not-found");
        // Correct code -> paired
        let res = backend
            .join_pairing_session(JoinPairingRequest::new(resp.code, PathBuf::from("/tmp/target-ok")))
            .await
            .unwrap();
        assert_eq!(res.status, "paired");
        assert_eq!(res.folder_id, "0123456789abcdef0123456789abcdef");
    });
}

#[test]
#[ignore = "requires full network setup; covered by in-memory handshake tests"]
fn loopback_e2e_pair_then_sync_trees_identical() {
    // This test would require two SyncEngines with paired folders to sync a file after pairing.
    // For wave 3 we verify in-memory handshake already ensures CONFIG_HEAD mutual; full loopback sync is exercised
    // in ferry-sync's convergence tests. Mark ignored per ticket: "can be marked ignored if needs network, but at least in-memory"
}
