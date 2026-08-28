use std::path::PathBuf;

use ferry_daemon::registry::{FolderRegistry, RegistryError};

fn tmp_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn round_trip_register_save_load() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().expect("dir_a");
    let mut reg = FolderRegistry::empty();
    let rec = reg.register(dir_a.path().to_path_buf()).expect("register");
    assert_eq!(rec.folder_id.len(), 64);
    assert!(rec.folder_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(ferry_platform::time::parse_rfc3339_to_unix(&rec.added_at).is_some());
    let folder_id = rec.folder_id.clone();
    reg.save(home.path()).expect("save");
    let loaded = FolderRegistry::load(home.path()).expect("load");
    assert_eq!(loaded.folders.len(), 1);
    assert_eq!(loaded.folders[0].folder_id, folder_id);
    assert_eq!(loaded.folders[0].path, dir_a.path());
    assert_eq!(loaded.folders[0].added_at, rec.added_at);
}

#[test]
fn validation_already_synced_child_inside_parent() {
    let dir_parent = tempfile::tempdir().expect("parent");
    let child = dir_parent.path().join("b");
    std::fs::create_dir_all(&child).expect("mkdir child");
    let mut reg = FolderRegistry::empty();
    reg.register(dir_parent.path().to_path_buf()).expect("register parent");
    let err = reg.register(child).expect_err("child should be rejected");
    assert_eq!(err.code, "already-synced");
}

#[test]
fn validation_already_synced_parent_contains_child() {
    let dir_parent = tempfile::tempdir().expect("parent");
    let child = dir_parent.path().join("child");
    std::fs::create_dir_all(&child).expect("mkdir child");
    let mut reg = FolderRegistry::empty();
    reg.register(child).expect("register child");
    let err = reg
        .register(dir_parent.path().to_path_buf())
        .expect_err("parent should be rejected");
    assert_eq!(err.code, "already-synced");
}

#[test]
fn validation_sibling_allowed() {
    let parent = tempfile::tempdir().expect("parent");
    let a = parent.path().join("a");
    let b = parent.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let mut reg = FolderRegistry::empty();
    reg.register(a).unwrap();
    let rec2 = reg.register(b).unwrap();
    assert_eq!(reg.folders.len(), 2);
    assert!(reg.folders.iter().any(|r| r.folder_id == rec2.folder_id));
}

#[test]
fn validation_bad_path_not_absolute() {
    let mut reg = FolderRegistry::empty();
    let err = reg
        .register(PathBuf::from("relative/path"))
        .expect_err("should reject relative");
    assert_eq!(err.code, "bad-path");
}

#[test]
fn validation_bad_path_not_exists() {
    let mut reg = FolderRegistry::empty();
    let err = reg
        .register(PathBuf::from("/tmp/ferry-registry-test-nonexistent-xyz-12345"))
        .expect_err("should reject missing");
    assert_eq!(err.code, "bad-path");
}

#[test]
fn validation_bad_path_not_a_dir() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut reg = FolderRegistry::empty();
    let err = reg
        .register(tmp.path().to_path_buf())
        .expect_err("should reject file");
    assert_eq!(err.code, "bad-path");
}

#[test]
fn atomicity_half_write_leaves_old_or_new() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut reg = FolderRegistry::empty();
    let rec_a = reg.register(dir_a.path().to_path_buf()).unwrap();
    reg.save(home.path()).unwrap();

    let folders_path = home.path().join("folders.toml");
    let original = std::fs::read_to_string(&folders_path).unwrap();

    let mut reg2 = FolderRegistry::load(home.path()).unwrap();
    let _rec_b = reg2.register(dir_b.path().to_path_buf()).unwrap();
    let new_content = toml::to_string(&reg2).unwrap();
    let half = &new_content[..new_content.len() / 2];
    {
        let mut tmp = tempfile::Builder::new()
            .prefix("folders")
            .suffix(".toml")
            .tempfile_in(home.path())
            .unwrap();
        use std::io::Write as _;
        tmp.write_all(half.as_bytes()).unwrap();
        tmp.flush().unwrap();
    }
    let loaded = FolderRegistry::load(home.path()).expect("load after half write attempt");
    assert_eq!(loaded.folders.len(), 1);
    assert_eq!(loaded.folders[0].folder_id, rec_a.folder_id);
    let current = std::fs::read_to_string(&folders_path).unwrap();
    assert_eq!(current, original);

    reg2.save(home.path()).unwrap();
    let loaded2 = FolderRegistry::load(home.path()).unwrap();
    assert_eq!(loaded2.folders.len(), 2);
}

#[test]
fn corrupt_toml_returns_corrupt_registry() {
    let home = tmp_home();
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(
        home.path().join("folders.toml"),
        "[[folders]]\nfolder_id = 123\npath = \"/tmp/a\"\nadded_at = \"2026-08-28T00:00:00Z\"\n",
    )
    .unwrap();
    let err = FolderRegistry::load(home.path()).expect_err("should be corrupt");
    assert_eq!(err.code, "corrupt-registry");
    assert!(
        err.hint.contains("fix or delete"),
        "hint should contain fix or delete: {}",
        err.hint
    );
    assert!(err.hint.contains("folders.toml"));
}

#[test]
fn corrupt_toml_non_hex_folder_id() {
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    let content = format!(
        "[[folders]]\nfolder_id = \"not-hex\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n",
        dir.path().display()
    );
    std::fs::write(home.path().join("folders.toml"), content).unwrap();
    let err = FolderRegistry::load(home.path()).unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn corrupt_toml_duplicate_folder_id() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dup_id = "aa".repeat(32);
    let content = format!(
        "[[folders]]\nfolder_id = \"{dup_id}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{dup_id}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
        dir_a.path().display(),
        dir_b.path().display()
    );
    std::fs::write(home.path().join("folders.toml"), content).unwrap();
    let err = FolderRegistry::load(home.path()).unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn corrupt_toml_overlapping_paths() {
    let home = tmp_home();
    let parent = tempfile::tempdir().unwrap();
    let child = parent.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let id1 = "aa".repeat(32);
    let id2 = "bb".repeat(32);
    let content = format!(
        "[[folders]]\nfolder_id = \"{id1}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{id2}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
        parent.path().display(),
        child.display()
    );
    std::fs::write(home.path().join("folders.toml"), content).unwrap();
    let err = FolderRegistry::load(home.path()).unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn concurrent_register_serializes_via_rename() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let mut reg1 = FolderRegistry::load(home.path()).unwrap();
    let rec_a = reg1.register(dir_a.path().to_path_buf()).unwrap();
    reg1.save(home.path()).unwrap();

    let mut reg2 = FolderRegistry::load(home.path()).unwrap();
    assert_eq!(reg2.folders.len(), 1);
    assert_eq!(reg2.folders[0].folder_id, rec_a.folder_id);
    let rec_b = reg2.register(dir_b.path().to_path_buf()).unwrap();
    reg2.save(home.path()).unwrap();

    let final_reg = FolderRegistry::load(home.path()).unwrap();
    assert_eq!(final_reg.folders.len(), 2);
    assert!(final_reg.folders.iter().any(|r| r.folder_id == rec_a.folder_id));
    assert!(final_reg.folders.iter().any(|r| r.folder_id == rec_b.folder_id));
}

#[test]
fn list_sorted_by_added_at() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();

    let content = format!(
        "[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:02Z\"\n[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
        "cc".repeat(32),
        dir_a.path().display(),
        "aa".repeat(32),
        dir_b.path().display(),
        "bb".repeat(32),
        dir_c.path().display()
    );
    std::fs::write(home.path().join("folders.toml"), content).unwrap();
    let loaded = FolderRegistry::load(home.path()).unwrap();
    let list = loaded.list();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].folder_id, "aa".repeat(32));
    assert_eq!(list[1].folder_id, "bb".repeat(32));
    assert_eq!(list[2].folder_id, "cc".repeat(32));
}

#[test]
fn remove_and_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = FolderRegistry::empty();
    let rec = reg.register(dir.path().to_path_buf()).unwrap();
    assert_eq!(reg.folders.len(), 1);
    reg.remove(&rec.folder_id).unwrap();
    assert_eq!(reg.folders.len(), 0);
    let err = reg.remove(&rec.folder_id).unwrap_err();
    assert_eq!(err.code, "not-found");
    let err2 = reg.remove("nonexistent").unwrap_err();
    assert_eq!(err2.code, "not-found");
}

#[test]
fn load_empty_when_missing() {
    let home = tmp_home();
    let reg = FolderRegistry::load(home.path()).unwrap();
    assert!(reg.folders.is_empty());
}

#[test]
fn save_creates_home_dir() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("new_home");
    assert!(!home.exists());
    let dir = tempfile::tempdir().unwrap();
    let mut reg = FolderRegistry::empty();
    reg.register(dir.path().to_path_buf()).unwrap();
    reg.save(&home).unwrap();
    assert!(home.join("folders.toml").exists());
}

#[test]
fn no_unwrap_on_corrupt_toml_garbage() {
    let home = tmp_home();
    std::fs::write(home.path().join("folders.toml"), "%%% garbage toml [[[ ").unwrap();
    let err = FolderRegistry::load(home.path()).unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
    assert!(err.hint.contains("fix or delete"));
}

#[test]
fn registry_error_to_op_error() {
    let err = RegistryError::new("bad-path", "bad", "hint");
    let op = err.to_op_error();
    assert_eq!(op.code, "bad-path");
    assert_eq!(op.hint, "hint");
}
