//! Folder bootstrap exercised directly through `ferry-folder` (ported from
//! the coverage that used to live only behind the CLI's library surface).

use std::path::Path;

use ferry_crypto::identity::load_or_create;
use ferry_folder::folder::{
    create_folder, dot_dir, find_polynomial, open_folder, save_settings, short_device,
    write_default_ignore_if_absent, Settings, SETTINGS_FORMAT_VERSION,
};
use ferry_store::store::Store;

const FOLDER_ID: [u8; 16] = [7u8; 16];
const POLY: u64 = 0x1234_5678_9ABC_DEF0;

/// Error-code assertion helper: bootstrap result types are plain structs
/// without Debug, so `unwrap_err` is not available.
#[track_caller]
fn code_of<T>(r: Result<T, ferry_folder::FolderError>) -> &'static str {
    match r {
        Err(e) => e.code,
        Ok(_) => panic!("expected an error"),
    }
}

fn identity_at(tag: &str) -> (tempfile::TempDir, ferry_crypto::identity::DeviceIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let identity = load_or_create(&dir.path().join(tag)).unwrap();
    (dir, identity)
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

/// init-equivalent: create + settings + flush, as every frontend's setup
/// does (the polynomial record sits in staging until the store flushes).
fn make_folder(root: &Path, id: &ferry_crypto::identity::DeviceIdentity) {
    let (store, _fmk) = create_folder(root, id, FOLDER_ID, POLY).unwrap();
    save_settings(root, &default_settings()).unwrap();
    write_default_ignore_if_absent(root).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
}

#[test]
fn create_then_open_round_trips_folder_id_and_polynomial() {
    let work = tempfile::tempdir().unwrap();
    let (_id_home, id) = identity_at("identity");
    let root = work.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();

    make_folder(&root, &id);

    // Spec layout exists.
    assert!(root.join(".ferry/config").is_file());
    assert!(root.join(".ferry/packs").is_dir());
    assert!(root.join(".ferry/index").is_dir());
    assert!(root.join(".ferry/settings.json").is_file());

    let opened = open_folder(&root, &id).expect("open succeeds");
    assert_eq!(opened.folder_id, FOLDER_ID);
    assert_eq!(opened.poly, POLY);
    assert_eq!(opened.settings.folder_id.len(), 32);
    assert_eq!(opened.root, root);
    assert_eq!(opened.state_dir(), dot_dir(&root));
    assert_eq!(opened.path(), root.as_path());

    // Polynomial lookup through the index finds exactly the stored record.
    assert_eq!(find_polynomial(&opened.store).unwrap(), POLY);
}

#[test]
fn open_without_ferry_directory_is_not_a_folder() {
    let work = tempfile::tempdir().unwrap();
    let (_home, id) = identity_at("identity");
    assert_eq!(code_of(open_folder(&work.path().join("empty"), &id)), "not-a-folder");
}

#[test]
fn open_rejects_a_device_the_folder_was_never_shared_with() {
    let work = tempfile::tempdir().unwrap();
    let (_owner_home, owner) = identity_at("owner");
    let (_stranger_home, stranger) = identity_at("stranger");
    let root = work.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();

    make_folder(&root, &owner);
    let err = open_folder(&root, &stranger).err().unwrap();
    assert_eq!(err.code, "not-shared-with-device");    // The display shorthand names the refused device.
    assert!(
        err.message.contains(&short_device(stranger.public())),
        "{}",
        err.message
    );
}

#[test]
fn adopt_writes_only_own_wrap_but_still_opens_cleanly() {
    let work = tempfile::tempdir().unwrap();
    let (_a_home, a) = identity_at("a");
    let (_b_home, b) = identity_at("b");

    // A owns a folder; B adopts its key material via the accept-side path.
    make_folder(&work.path().join("owned"), &a);
    let fmk = ferry_crypto::folder_key::generate_fmk();
    let target = work.path().join("adopted");
    let store: Store = ferry_folder::folder::adopt_folder(&target, &b, FOLDER_ID, &fmk, POLY)
        .expect("adopt succeeds");

    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    // The accepting frontend persists settings after adopting (the ritual
    // does this inside accept_complete); mirror that here.
    ferry_folder::folder::save_settings(&target, &default_settings()).unwrap();

    // B opens its adopted copy; A cannot.
    let opened_b = open_folder(&target, &b).expect("adopter opens");
    assert_eq!(opened_b.folder_id, FOLDER_ID);
    assert_eq!(code_of(open_folder(&target, &a)), "not-shared-with-device");

    // Adopted CONFIG_HEAD holds exactly one wrap (the adopter's own).
    let head = ferry_crypto::config_head::parse_config_head(
        &std::fs::read(target.join(".ferry/config")).unwrap(),
    )
    .unwrap();
    assert_eq!(head.entries.len(), 1);
    assert_eq!(&head.entries[0].device_pub, b.public());
}

#[test]
fn settings_survive_save_load_with_stable_format_version() {
    let work = tempfile::tempdir().unwrap();
    let (_home, id) = identity_at("identity");
    let root = work.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    make_folder(&root, &id);

    let raw = std::fs::read_to_string(dot_dir(&root).join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["format_version"], SETTINGS_FORMAT_VERSION);
    assert_eq!(v["folder_id"], ferry_store::format::hex(&FOLDER_ID));

    let back: Settings = serde_json::from_str(&raw).unwrap();
    assert_eq!(back, default_settings());
    assert!(back.ignore_config().presets.is_empty());
}

#[test]
fn default_ignore_file_written_once() {
    let work = tempfile::tempdir().unwrap();
    assert!(write_default_ignore_if_absent(work.path()).unwrap());
    assert!(!write_default_ignore_if_absent(work.path()).unwrap());
    assert!(work.path().join("ferry.ignore").is_file());
}

#[test]
fn double_initialize_refuses_through_already_initialized_code() {
    let work = tempfile::tempdir().unwrap();
    let (_home, id) = identity_at("identity");
    let root = work.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    make_folder(&root, &id);
    let err = code_of(create_folder(&root, &id, FOLDER_ID, POLY));
    assert_eq!(err, "already-initialized");
}
