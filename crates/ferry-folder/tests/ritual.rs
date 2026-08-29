//! The unified pairing ritual exercised end to end through `ferry-folder`,
//! both devices simulated in-process. Every test drives ONLY the public
//! `PairingRitual` seam — no internal intermediate structs, no transport
//! branching: the input form (6-char code vs `FERRY1:` envelope vs payload
//! file path) is the only thing a caller ever chooses.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ferry_crypto::identity::{load_or_create, DeviceIdentity};
use ferry_folder::folder::{
    create_folder, dot_dir, open_folder, save_settings, OpenFolder, Settings,
    SETTINGS_FORMAT_VERSION,
};
use ferry_folder::pairing::{
    new_shared_rendezvous, PairingCompleted, PairingRitual, PendingAcceptance, PendingOffer,
    SharedRendezvous, GRANT_SUFFIX, OFFER_SUFFIX, PAYLOAD_PREFIX, RESPONSE_SUFFIX,
};

const FOLDER_ID: [u8; 16] = [9u8; 16];
const POLY: u64 = 0x0FED_CBA9_8765_4321;

/// Error-code assertion helper: the ritual's result types are plain structs
/// without Debug, so `unwrap_err` is not available.
#[track_caller]
fn code_of<T>(r: Result<T, ferry_folder::FolderError>) -> &'static str {
    match r {
        Err(e) => e.code,
        Ok(_) => panic!("expected an error"),
    }
}

/// Keeps the identity tempdir alive for the test's duration.
struct Home(tempfile::TempDir);

impl Home {
    fn path(&self) -> &std::path::Path {
        self.0.path()
    }
}

fn identity(tag: &str) -> (Home, DeviceIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let id = load_or_create(&dir.path().join(tag)).unwrap();
    (Home(dir), id)
}

fn default_settings() -> Settings {
    Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&FOLDER_ID),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    }
}

/// Device A's side of setup: an owned, opened folder.
fn opened_owner_folder(root: &Path, id: &DeviceIdentity) -> OpenFolder {
    std::fs::create_dir_all(root).unwrap();
    let (store, _fmk) = create_folder(root, id, FOLDER_ID, POLY).unwrap();
    save_settings(root, &default_settings()).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    open_folder(root, id).unwrap()
}

/// One ritual per device over an ISOLATED rendezvous map.
fn ritual(home: &Path, id: &DeviceIdentity, rendezvous: &SharedRendezvous) -> PairingRitual {
    PairingRitual::with_shared(home.to_path_buf(), id.clone(), Arc::clone(rendezvous))
}

use std::sync::Arc;

fn assert_both_configs_name_both_devices(
    root_a: &Path,
    root_b: &Path,
    pub_a: &[u8; 32],
    id_b: &DeviceIdentity,
) {
    for root in [root_a, root_b] {
        let head = ferry_crypto::config_head::parse_config_head(
            &std::fs::read(dot_dir(root).join("config")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.entries.len(), 2, "{}", root.display());
        let pubs: Vec<_> = head.entries.iter().map(|e| e.device_pub).collect();
        assert!(pubs.contains(pub_a));
        assert!(pubs.contains(id_b.public()));
    }
}

#[test]
fn rendezvous_code_completes_pairing_without_any_payload_files() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i: PendingOffer = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();

    // The offer is a 6-character code; nothing landed in the folder yet.
    assert_eq!(pending_i.short_code.len(), 6);
    assert!(pending_i
        .short_code
        .chars()
        .all(|c| ferry_crypto::base32::ALPHABET.contains(&(c as u8))));
    assert!(
        !pending_i.payload_path.exists(),
        "no artifact before complete"
    );

    // B dials the SAME code through its own ritual. No file ever exists.
    let pending_b: PendingAcceptance = ritual(home_b.path(), &id_b, &rendezvous)
        .accept_offer(&pending_i.short_code, Some(&target_b))
        .unwrap();
    assert_eq!(pending_b.expected_short_code, pending_i.short_code);
    assert!(
        pending_b.response_path.is_none(),
        "in-band: no response file"
    );
    let accepted = pending_b.complete(0).unwrap();

    assert_eq!(accepted.folder_id, FOLDER_ID);
    assert!(!dot_dir(&root_a).join(OFFER_SUFFIX).exists());
    assert!(!dot_dir(&root_a).join(RESPONSE_SUFFIX).exists());
    assert!(!dot_dir(&root_a).join(GRANT_SUFFIX).exists());

    assert_both_configs_name_both_devices(&root_a, &accepted.folder, id_a.public(), &id_b);

    // B's adopted folder opens with its own identity and matches the poly.
    let opened_b = open_folder(accepted.folder.as_path(), &id_b).unwrap();
    assert_eq!(opened_b.poly, POLY);
    assert!(accepted.folder.join(".ferry/index").is_dir());

    // One-time session: the code no longer dials.
    let err = code_of(
        ritual(home_b.path(), &id_b, &rendezvous)
            .accept_offer(&pending_i.short_code, Some(&work.path().join("again"))),
    );
    assert_eq!(err, "pairing-not-found");
}

#[test]
fn payload_file_exchange_completes_pairing_beside_the_offer() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    let pub_a = *id_a.public();
    let root_for_thread = root_a.clone();
    let opened_for_thread = open_folder(&root_for_thread, &id_a).unwrap();
    let id_a_for_thread = id_a.clone();
    let short_code = pending_i.short_code.clone();

    // A engages the file transport on a thread: writes the payload file and
    // polls for B's response beside it.
    let handle = std::thread::spawn(move || -> PairingCompleted {
        pending_i
            .complete(&opened_for_thread, &id_a_for_thread, 30)
            .expect("initiate")
    });

    // B answers the payload file path — the out-of-band act (AirDrop/scp)
    // stands in for the copy between machines.
    let offer_file = dot_dir(&root_a).join(OFFER_SUFFIX);
    // The file appears once A's thread starts; poll briefly.
    let mut deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !offer_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    deadline = std::time::Instant::now() + Duration::from_secs(5);
    let pending_b = loop {
        match ritual(home_b.path(), &id_b, &rendezvous)
            .accept_offer(&offer_file.display().to_string(), Some(&target_b))
        {
            Ok(p) => break p,
            Err(e) if e.code == "not-found" && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("accept failed: {} {}", e.code, e.message),
        }
    };

    // Both humans see the SAME code, and B's response landed beside the
    // offer where A's poller looks.
    assert_eq!(pending_b.expected_short_code, short_code);
    assert_eq!(
        pending_b.response_path.as_ref().unwrap(),
        &offer_file.with_file_name(RESPONSE_SUFFIX)
    );
    assert!(pending_b.response_path.as_ref().unwrap().is_file());

    let accepted = pending_b.complete(30).unwrap();
    let completed = handle.join().unwrap();

    assert_eq!(accepted.folder_id, FOLDER_ID);
    assert_eq!(completed.folder_id, FOLDER_ID);
    assert_eq!(completed.short_code, short_code);
    let dot = dot_dir(&root_a);
    assert!(dot.join(OFFER_SUFFIX).is_file());
    assert!(dot.join(RESPONSE_SUFFIX).is_file());
    assert!(dot.join(GRANT_SUFFIX).is_file());

    assert_both_configs_name_both_devices(&root_for_thread, &accepted.folder, &pub_a, &id_b);
    let opened_b = open_folder(accepted.folder.as_path(), &id_b).unwrap();
    assert_eq!(opened_b.poly, POLY);
}

#[test]
fn envelope_parses_digit_bearing_base32_codes() {
    // The Base32 alphabet is `23456789ABCDEFGH...` — codes routinely carry
    // digits. Regression: the envelope parser once accepted letters only,
    // breaking the file transport for ~80% of generated codes.
    let envelope = format!("FERRY1:XUM5CA:{}:1788082604", "ab".repeat(93));
    let parsed = ferry_folder::pairing::parse_payload_envelope(&envelope).expect("parses");
    assert_eq!(parsed.code, "XUM5CA");
    assert_eq!(parsed.offer_bytes.len(), 93);
}

#[test]
fn envelope_text_answer_in_band_without_a_file() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();

    // The QR payload IS the envelope; B pastes it verbatim.
    let envelope = pending_i.qr_payload();
    assert!(envelope.starts_with(PAYLOAD_PREFIX));

    let pending_b = ritual(home_b.path(), &id_b, &rendezvous)
        .accept_offer(&envelope, Some(&target_b))
        .unwrap();
    let accepted = pending_b.complete(0).unwrap();
    assert_eq!(accepted.folder_id, FOLDER_ID);
    assert_both_configs_name_both_devices(&root_a, &accepted.folder, id_a.public(), &id_b);
}

#[test]
fn wrong_or_mistyped_codes_fail_with_pairing_not_found() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();

    for bad in ["ZZZZZZ", "WRONG1", "ABC-123"] {
        let err =
            code_of(ritual(home_b.path(), &id_b, &rendezvous).accept_offer(bad, Some(&target_b)));
        assert_eq!(err, "pairing-not-found", "{bad}");
    }

    // The real code still dials after failed attempts.
    let pending_b = ritual(home_b.path(), &id_b, &rendezvous)
        .accept_offer(&pending_i.short_code, Some(&target_b))
        .unwrap();
    assert_eq!(pending_b.complete(0).unwrap().folder_id, FOLDER_ID);
}

#[test]
fn expired_code_is_refused_and_consumed() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();

    // Force expiry through the shared rendezvous seam (the same public map
    // production daemons share across frontends).
    let key = pending_i.short_code.to_ascii_uppercase();
    rendezvous.lock().unwrap().get_mut(&key).unwrap().expires_at =
        SystemTime::now() - Duration::from_secs(1);

    let err = code_of(
        ritual(home_b.path(), &id_b, &rendezvous)
            .accept_offer(&pending_i.short_code, Some(&target_b)),
    );
    assert_eq!(err, "pairing-expired");
    assert!(!rendezvous.lock().unwrap().contains_key(&key), "consumed");
}

#[test]
fn accept_refuses_an_already_initialized_target() {
    let work = tempfile::tempdir().unwrap();
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();
    let root_a = work.path().join("owned");
    let target_b = work.path().join("already");

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    // Target already has a .ferry store.
    std::fs::create_dir_all(dot_dir(&target_b)).unwrap();

    let err = code_of(
        ritual(home_b.path(), &id_b, &rendezvous)
            .accept_offer(&pending_i.short_code, Some(&target_b)),
    );
    assert_eq!(err, "already-initialized");
}

#[test]
fn initiate_times_out_without_a_responder() {
    let work = tempfile::tempdir().unwrap();
    let (home_a, id_a) = identity("a");
    let rendezvous = new_shared_rendezvous();
    let root_a = work.path().join("owned");
    let opened_a = opened_owner_folder(&root_a, &id_a);

    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    let err = code_of(pending_i.complete(&opened_a, &id_a, 0));
    assert_eq!(err, "pair-timeout");
    // The payload file itself WAS written before polling.
    assert!(dot_dir(&root_a).join(OFFER_SUFFIX).is_file());
}

#[test]
fn accept_times_out_without_a_grant() {
    let work = tempfile::tempdir().unwrap();
    let (home_a, id_a) = identity("a");
    let (home_b, id_b) = identity("b");
    let rendezvous = new_shared_rendezvous();
    let root_a = work.path().join("owned");
    let target_b = work.path().join("fresh");

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = ritual(home_a.path(), &id_a, &rendezvous)
        .create_offer(&opened_a)
        .unwrap();
    // The out-of-band act: the payload file exists, but A never finishes.
    let pending = &pending_i;
    std::fs::write(&pending.payload_path, &pending.payload).unwrap();

    let pending_b = ritual(home_b.path(), &id_b, &rendezvous)
        .accept_offer(&pending.payload_path.display().to_string(), Some(&target_b))
        .unwrap();
    assert!(pending_b.response_path.as_ref().unwrap().is_file());
    assert_eq!(pending_b.expected_short_code, pending_i.short_code);

    let err = code_of(pending_b.complete(0));
    assert_eq!(err, "pair-timeout");
    let _ = UNIX_EPOCH; // keep the import honest if expiry assertions move
}
