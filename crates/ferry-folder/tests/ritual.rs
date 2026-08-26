//! The pairing ritual exercised directly through `ferry-folder`, both
//! devices simulated in-process (ported from the coverage that used to live
//! only behind the CLI's binary and library surface).

use std::path::Path;

use ferry_crypto::identity::{load_or_create, DeviceIdentity};
use ferry_folder::folder::{
    create_folder, dot_dir, open_folder, save_settings, OpenFolder, Settings,
    SETTINGS_FORMAT_VERSION,
};
use ferry_folder::pairing::{
    accept_begin, accept_complete, initiate_begin, initiate_complete, Accepted, PairingCompleted,
    PendingAcceptance, PendingOffer, GRANT_SUFFIX, OFFER_SUFFIX, RESPONSE_SUFFIX,
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
struct Home(#[allow(dead_code)] tempfile::TempDir);

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

#[test]
fn full_ritual_two_devices_adopt_same_folder() {
    let work = tempfile::tempdir().unwrap();
    let root_a = work.path().join("device-a");
    let target_b = work.path().join("device-b");
    let (_home_a, id_a) = identity("a");
    let (_home_b, id_b) = identity("b");

    let opened_a = opened_owner_folder(&root_a, &id_a);

    // A begins; nothing is on disk yet.
    let pending_i: PendingOffer = initiate_begin(&opened_a, &id_a).expect("begin");
    assert!(pending_i.offer_bytes.starts_with(b"FRPO"));
    assert_eq!(pending_i.offer_path, dot_dir(&root_a).join(OFFER_SUFFIX));
    assert!(!pending_i.offer_path.exists(), "no artifact before complete");
    assert!(!pending_i.short_code.is_empty());
    let pub_a = *id_a.public();
    let root_for_thread = root_a.clone();

    // B answers the offer file path A will write. The out-of-band act
    // stands in for the copy between machines: place the offer where B
    // can read it, as `initiate_complete` would.
    std::fs::write(&pending_i.offer_path, &pending_i.offer_bytes).unwrap();
    let pending_a: PendingAcceptance =
        accept_begin(&id_b, &pending_i.offer_path, Some(&target_b)).expect("accept begin");
    assert!(!dot_dir(&target_b).exists(), "target untouched until grant");
    assert_eq!(
        pending_a.response_path,
        pending_i.offer_path.with_file_name(RESPONSE_SUFFIX)
    );

    // A completes on a thread once the response exists; B waits for the grant.
    let handle = std::thread::spawn(move || -> (PairingCompleted, OpenFolder) {
        let done = initiate_complete(pending_i, &opened_a, &id_a, 30).expect("initiate");
        (done, open_folder(&root_for_thread, &id_a).unwrap())
    });
    let accepted: Accepted = accept_complete(pending_a, &id_b, 30).expect("accept");

    // Both sides agree on the folder identity and polynomial.
    let (completed, reopened_a) = handle.join().unwrap();
    assert_eq!(accepted.folder_id, FOLDER_ID);
    assert_eq!(completed.folder_id, FOLDER_ID);
    assert_eq!(completed.peer_device_id, *id_b.public());
    assert_eq!(reopened_a.poly, POLY);

    // Frozen artifact names, all three present beside each other.
    let dot = dot_dir(&root_a);
    assert!(dot.join(OFFER_SUFFIX).is_file());
    assert!(dot.join(RESPONSE_SUFFIX).is_file());
    assert_eq!(completed.grant_path, dot.join(GRANT_SUFFIX));
    assert!(dot.join(GRANT_SUFFIX).is_file());

    // Each CONFIG_HEAD names BOTH devices (the engine's peer allow-list).
    for root in [&root_a, &accepted.folder] {
        let head = ferry_crypto::config_head::parse_config_head(
            &std::fs::read(dot_dir(root).join("config")).unwrap(),
        )
        .unwrap();
        assert_eq!(head.entries.len(), 2, "{root:?}");
        let pubs: Vec<_> = head.entries.iter().map(|e| e.device_pub).collect();
        assert!(pubs.contains(&pub_a));
        assert!(pubs.contains(id_b.public()));
    }

    // B's adopted folder opens with its own identity and matches the poly.
    let opened_b = open_folder(accepted.folder.as_path(), &id_b).unwrap();
    assert_eq!(opened_b.poly, POLY);
    assert_eq!(opened_b.settings.folder_id, ferry_store::format::hex(&FOLDER_ID));

    // Adopted store flushed to disk: index snapshot readable via reopen.
    assert!(accepted.folder.join(".ferry/index").is_dir());
}

#[test]
fn accept_refuses_an_already_initialized_target() {
    let work = tempfile::tempdir().unwrap();
    let (_a_home, id_a) = identity("a");
    let (_b_home, id_b) = identity("b");
    let root_a = work.path().join("owned");
    let target_b = work.path().join("already");

    let opened_a = opened_owner_folder(&root_a, &id_a);
    // Target already has a .ferry store.
    std::fs::create_dir_all(dot_dir(&target_b)).unwrap();

    let pending_i = initiate_begin(&opened_a, &id_a).unwrap();
    // Write the offer ourselves so accept_begin can read it.
    std::fs::write(&pending_i.offer_path, &pending_i.offer_bytes).unwrap();

    let err = code_of(accept_begin(&id_b, &pending_i.offer_path, Some(&target_b)));
    assert_eq!(err, "already-initialized");
}

#[test]
fn initiate_times_out_without_a_responder() {
    let work = tempfile::tempdir().unwrap();
    let (_home_a, id_a) = identity("a");
    let root_a = work.path().join("owned");
    let opened_a = opened_owner_folder(&root_a, &id_a);

    let pending_i = initiate_begin(&opened_a, &id_a).unwrap();
    let err = code_of(initiate_complete(pending_i, &opened_a, &id_a, 0));
    assert_eq!(err, "pair-timeout");
    // The offer file itself WAS written before polling.
    assert!(dot_dir(&root_a).join(OFFER_SUFFIX).is_file());
}

#[test]
fn accept_times_out_without_a_grant() {
    let work = tempfile::tempdir().unwrap();
    let (_a_home, id_a) = identity("a");
    let (_b_home, id_b) = identity("b");
    let root_a = work.path().join("owned");
    let target_b = work.path().join("fresh");

    let opened_a = opened_owner_folder(&root_a, &id_a);
    let pending_i = initiate_begin(&opened_a, &id_a).unwrap();
    std::fs::write(&pending_i.offer_path, &pending_i.offer_bytes).unwrap();

    let pending_a = accept_begin(&id_b, &pending_i.offer_path, Some(&target_b)).unwrap();
    assert!(pending_a.response_path.is_file());
    assert!(!pending_a.expected_short_code.is_empty());

    let err = code_of(accept_complete(pending_a, &id_b, 0));
    assert_eq!(err, "pair-timeout");
}
