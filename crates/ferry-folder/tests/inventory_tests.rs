use std::path::PathBuf;
use std::thread;

use ferry_folder::inventory::{validate_path, FolderInventory};

fn tmp_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn register_persists_and_lists_sorted() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().expect("dir_a");
    let dir_b = tempfile::tempdir().expect("dir_b");

    let inv = FolderInventory::new(home.path());
    let rec = inv.register(dir_a.path()).expect("register a");
    assert_eq!(rec.folder_id.len(), 64);
    assert!(rec.folder_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(rec.path, dir_a.path());
    assert!(ferry_platform::time::parse_rfc3339_to_unix(&rec.added_at).is_some());

    let rec_b = FolderInventory::new(home.path())
        .register(dir_b.path())
        .expect("register b");

    let list = FolderInventory::new(home.path()).list().expect("list");
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|r| r.folder_id == rec.folder_id));
    assert!(list.iter().any(|r| r.folder_id == rec_b.folder_id));
}

#[test]
fn register_survives_reopen_round_trip() {
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    let rec = FolderInventory::new(home.path())
        .register(dir.path())
        .expect("register");

    let loaded = FolderInventory::new(home.path())
        .list()
        .expect("load after reopen");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].folder_id, rec.folder_id);
    assert_eq!(loaded[0].path, dir.path());
    assert_eq!(loaded[0].added_at, rec.added_at);
}

#[test]
fn register_rejects_relative_missing_and_file_paths() {
    let home = tmp_home();
    let inv = FolderInventory::new(home.path());

    let err = inv.register(&PathBuf::from("relative/path")).unwrap_err();
    assert_eq!(err.code, "bad-path");

    let err = inv
        .register(&PathBuf::from(
            "/tmp/ferry-inventory-test-nonexistent-xyz-12345",
        ))
        .unwrap_err();
    assert_eq!(err.code, "bad-path");

    let file = tempfile::NamedTempFile::new().unwrap();
    let err = inv.register(file.path()).unwrap_err();
    assert_eq!(err.code, "bad-path");

    assert!(inv.list().unwrap().is_empty());
}

#[test]
fn register_rejects_child_inside_registered_parent() {
    let home = tmp_home();
    let parent = tempfile::tempdir().unwrap();
    let child = parent.path().join("b");
    std::fs::create_dir_all(&child).unwrap();

    let inv = FolderInventory::new(home.path());
    inv.register(parent.path()).expect("register parent");
    let err = inv.register(&child).expect_err("child should be rejected");
    assert_eq!(err.code, "already-synced");
}

#[test]
fn register_rejects_parent_of_registered_child() {
    let home = tmp_home();
    let parent = tempfile::tempdir().unwrap();
    let child = parent.path().join("child");
    std::fs::create_dir_all(&child).unwrap();

    let inv = FolderInventory::new(home.path());
    inv.register(&child).expect("register child");
    let err = inv
        .register(parent.path())
        .expect_err("parent should be rejected");
    assert_eq!(err.code, "already-synced");
}

#[test]
fn register_allows_siblings() {
    let home = tmp_home();
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("a");
    let b = parent.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    let inv = FolderInventory::new(home.path());
    inv.register(&a).unwrap();
    inv.register(&b).expect("siblings are fine");
    assert_eq!(inv.list().unwrap().len(), 2);
}

#[test]
fn unregister_removes_then_not_found() {
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    let inv = FolderInventory::new(home.path());
    let rec = inv.register(dir.path()).unwrap();
    assert_eq!(inv.list().unwrap().len(), 1);

    inv.unregister(&rec.folder_id).expect("unregister");
    assert!(inv.list().unwrap().is_empty());

    let err = inv.unregister(&rec.folder_id).unwrap_err();
    assert_eq!(err.code, "not-found");
    let err = inv.unregister("nonexistent").unwrap_err();
    assert_eq!(err.code, "not-found");
}

#[test]
fn atomic_persistence_concurrent_registers_all_land() {
    let home = tmp_home();
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();

    let home_path = home.path().to_path_buf();
    let handles: Vec<_> = dirs
        .iter()
        .map(|d| {
            let home = home_path.clone();
            let dir = d.path().to_path_buf();
            thread::spawn(move || {
                FolderInventory::new(&home)
                    .register(&dir)
                    .expect("concurrent register")
            })
        })
        .collect();
    let mut ids = Vec::new();
    for h in handles {
        ids.push(h.join().expect("thread").folder_id);
    }

    let final_list = FolderInventory::new(home.path())
        .list()
        .expect("final load");
    assert_eq!(final_list.len(), dirs.len(), "no registration may be lost");
    for id in &ids {
        assert!(final_list.iter().any(|r| &r.folder_id == id));
    }
}

#[test]
fn atomic_persistence_interrupted_write_keeps_old_registry() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let inv = FolderInventory::new(home.path());
    let rec_a = inv.register(dir_a.path()).unwrap();

    let folders_path = home.path().join("folders.toml");

    let inv2 = FolderInventory::new(home.path());
    let rec_b = inv2.register(dir_b.path()).unwrap();
    let new_content = std::fs::read_to_string(&folders_path).unwrap();
    assert!(new_content.contains(&rec_b.folder_id[..32]));

    {
        let mut tmp = tempfile::Builder::new()
            .prefix("folders")
            .suffix(".toml")
            .tempfile_in(home.path())
            .unwrap();
        use std::io::Write as _;
        tmp.write_all(&new_content.as_bytes()[..new_content.len() / 2])
            .unwrap();
        tmp.flush().unwrap();
    }

    let loaded = FolderInventory::new(home.path()).list().unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded.iter().any(|r| r.folder_id == rec_a.folder_id));
    assert!(loaded.iter().any(|r| r.folder_id == rec_b.folder_id));
    assert_eq!(
        std::fs::read_to_string(&folders_path).unwrap(),
        new_content,
        "half-written temp file must not replace the registry"
    );
}

#[test]
fn save_creates_missing_home_dir() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("new_home");
    assert!(!home.exists());
    let dir = tempfile::tempdir().unwrap();
    FolderInventory::new(&home).register(dir.path()).unwrap();
    assert!(home.join("folders.toml").exists());
}

fn write_registry(home: &std::path::Path, content: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(home.join("folders.toml"), content).unwrap();
}

#[test]
fn corrupt_toml_is_a_hard_error() {
    let home = tmp_home();
    write_registry(
        home.path(),
        "[[folders]]\nfolder_id = 123\npath = \"/tmp/a\"\nadded_at = \"2026-08-28T00:00:00Z\"\n",
    );
    let err = FolderInventory::new(home.path())
        .list()
        .expect_err("should be corrupt");
    assert_eq!(err.code, "corrupt-registry");
    assert!(err.hint.contains("fix or delete"), "hint: {}", err.hint);
    assert!(err.hint.contains("folders.toml"));
}

#[test]
fn corrupt_toml_garbage_is_a_hard_error() {
    let home = tmp_home();
    write_registry(home.path(), "%%% garbage toml [[[ ");
    let err = FolderInventory::new(home.path()).list().unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
    assert!(err.hint.contains("fix or delete"));
}

#[test]
fn corrupt_toml_non_hex_folder_id() {
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    write_registry(
        home.path(),
        &format!(
            "[[folders]]\nfolder_id = \"not-hex\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n",
            dir.path().display()
        ),
    );
    let err = FolderInventory::new(home.path()).list().unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn corrupt_toml_duplicate_folder_id() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dup_id = "aa".repeat(32);
    write_registry(
        home.path(),
        &format!(
            "[[folders]]\nfolder_id = \"{dup_id}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{dup_id}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
            dir_a.path().display(),
            dir_b.path().display()
        ),
    );
    let err = FolderInventory::new(home.path()).list().unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn corrupt_toml_overlapping_paths() {
    let home = tmp_home();
    let parent = tempfile::tempdir().unwrap();
    let child = parent.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    write_registry(
        home.path(),
        &format!(
            "[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
            "aa".repeat(32),
            parent.path().display(),
            "bb".repeat(32),
            child.display()
        ),
    );
    let err = FolderInventory::new(home.path()).list().unwrap_err();
    assert_eq!(err.code, "corrupt-registry");
}

#[test]
fn list_sorted_by_added_at() {
    let home = tmp_home();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();
    write_registry(
        home.path(),
        &format!(
            "[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:02Z\"\n[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:00Z\"\n[[folders]]\nfolder_id = \"{}\"\npath = \"{}\"\nadded_at = \"2026-08-28T00:00:01Z\"\n",
            "cc".repeat(32),
            dir_a.path().display(),
            "aa".repeat(32),
            dir_b.path().display(),
            "bb".repeat(32),
            dir_c.path().display()
        ),
    );
    let list = FolderInventory::new(home.path()).list().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].folder_id, "aa".repeat(32));
    assert_eq!(list[1].folder_id, "bb".repeat(32));
    assert_eq!(list[2].folder_id, "cc".repeat(32));
}

#[test]
fn load_empty_when_missing() {
    let home = tmp_home();
    assert!(FolderInventory::new(home.path()).list().unwrap().is_empty());
}

#[test]
fn inspect_dir_classifies_entries_in_one_pass() {
    let home = tmp_home();
    let inv = FolderInventory::new(home.path());
    let root = tempfile::tempdir().unwrap();

    std::fs::create_dir(root.path().join("z_dir")).unwrap();
    std::fs::create_dir(root.path().join("a_dir")).unwrap();
    std::fs::create_dir(root.path().join("a_dir").join(".git")).unwrap();
    std::fs::write(root.path().join("m_file.txt"), b"x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("a_dir"), root.path().join("l_link")).unwrap();

    let synced = root.path().join("z_dir");
    let rec = inv.register(&synced).unwrap();

    let resp = inv
        .inspect_dir(Some(root.path().to_path_buf()))
        .expect("inspect");
    assert_eq!(resp.absolute_path, root.path());

    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();

    assert_eq!(names, vec!["a_dir", "l_link", "z_dir", "m_file.txt"]);

    let a = &resp.entries[0];
    assert!(a.is_dir);
    assert!(a.is_git_repo, "dir containing .git is a git repo");
    assert!(!a.is_already_synced);

    #[cfg(unix)]
    {
        let l = &resp.entries[1];
        assert!(l.is_symlink);
    }

    let z = &resp.entries[2];
    assert!(z.is_dir);
    assert!(!z.is_git_repo);
    assert!(z.is_already_synced, "registered folder is marked synced");

    let f = &resp.entries[3];
    assert!(!f.is_dir);
    assert!(!f.is_git_repo);

    std::fs::create_dir(synced.join("sub")).unwrap();
    let inner = inv.inspect_dir(Some(synced.clone())).unwrap();
    let sub = inner.entries.iter().find(|e| e.name == "sub").unwrap();
    assert!(sub.is_already_synced);
    let _ = rec;
}

#[test]
fn inspect_dir_defaults_and_errors() {
    let home = tmp_home();
    let inv = FolderInventory::new(home.path());

    let file = tempfile::NamedTempFile::new().unwrap();
    let err = inv
        .inspect_dir(Some(file.path().to_path_buf()))
        .unwrap_err();
    assert_eq!(err.code, "not-a-directory");

    let err = inv
        .inspect_dir(Some(PathBuf::from("/tmp/ferry-inspect-nope-xyz")))
        .unwrap_err();
    assert_eq!(err.code, "not-found");

    let err = inv
        .inspect_dir(Some(PathBuf::from("/tmp/../etc/passwd")))
        .unwrap_err();
    assert_eq!(err.code, "path-traversal");
    assert_eq!(err.hint, "path escapes allowed root");
}

#[test]
fn inspect_dir_survives_corrupt_registry() {
    let home = tmp_home();
    write_registry(home.path(), "%%% garbage toml [[[ ");
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("f.txt"), b"x").unwrap();

    let inv = FolderInventory::new(home.path());
    let resp = inv
        .inspect_dir(Some(root.path().to_path_buf()))
        .expect("inspect still works");
    assert_eq!(resp.entries.len(), 1);
    assert!(!resp.entries[0].is_already_synced);
}

#[test]
fn inspect_dir_carries_initialization_verdict_per_entry() {
    let home = tmp_home();
    let inv = FolderInventory::new(home.path());
    let root = tempfile::tempdir().unwrap();

    std::fs::create_dir(root.path().join("plain")).unwrap();
    let dot_only = root.path().join("dot_only");
    std::fs::create_dir_all(dot_only.join(".ferry")).unwrap();
    let ready = root.path().join("ready");
    std::fs::create_dir_all(ready.join(".ferry")).unwrap();
    std::fs::write(ready.join(".ferry").join("config"), b"head").unwrap();
    std::fs::write(root.path().join("f.txt"), b"x").unwrap();

    let resp = inv
        .inspect_dir(Some(root.path().to_path_buf()))
        .expect("inspect");
    let initialized = |name: &str| {
        resp.entries
            .iter()
            .find(|e| e.name == name)
            .unwrap()
            .is_initialized
    };
    assert!(!initialized("plain"), "no .ferry at all");
    assert!(
        !initialized("dot_only"),
        ".ferry without config is not initialized"
    );
    assert!(initialized("ready"));
    assert!(!initialized("f.txt"), "files are never initialized");
}

#[test]
fn validate_path_guards() {
    let p = validate_path(Some(PathBuf::from("/tmp/foo"))).unwrap();
    assert_eq!(p, PathBuf::from("/tmp/foo"));

    let e = validate_path(Some(PathBuf::from("/tmp/../etc/passwd"))).unwrap_err();
    assert_eq!(e.code, "path-traversal");
    assert_eq!(e.hint, "path escapes allowed root");

    let e = validate_path(Some(PathBuf::from("relative"))).unwrap_err();
    assert_eq!(e.code, "bad-path");

    let e = validate_path(Some(PathBuf::from("/tmp//foo"))).unwrap_err();
    assert_eq!(e.code, "bad-path");

    let p = validate_path(None).unwrap();
    assert!(p.is_absolute());
}

#[test]
fn validate_path_nfc_normalizes() {
    let decomposed = "e\u{0301}";
    let p = validate_path(Some(PathBuf::from(format!("/tmp/{decomposed}")))).unwrap();
    assert!(p.to_string_lossy().contains('é'));
}
