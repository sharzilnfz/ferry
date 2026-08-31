use std::path::{Path, PathBuf};

use ferry_store::format::hex;
use unicode_normalization::UnicodeNormalization;

pub fn device_short(device: &[u8; 32]) -> String {
    hex(device)[..8].to_string()
}

pub fn conflict_display_name(
    original_name: &str,
    loser_device: &[u8; 32],
    loser_mtime_sec: i64,
) -> String {
    let nfc: String = original_name.nfc().collect();
    format!(
        "{nfc}.ferry-conflict.{}-{}",
        device_short(loser_device),
        ferry_platform::time::fmt_compact(loser_mtime_sec)
    )
}

pub fn unique_conflict_dest(
    root: &Path,
    relative_parent: &[String],
    candidate_base: &str,
) -> std::io::Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for c in relative_parent {
        dir.push(c);
    }
    let mut counter: u32 = 1;
    loop {
        let name = if counter == 1 {
            candidate_base.to_string()
        } else {
            format!("{candidate_base}-{counter}")
        };
        let abs = dir.join(name);
        match std::fs::symlink_metadata(&abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(abs),
            Err(e) => return Err(e),
            Ok(_) => counter += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_loser_short_id_and_loser_mtime() {
        let dev = [0xAB; 32];
        let name = conflict_display_name("notes.txt", &dev, 1_787_574_896);
        assert_eq!(name, "notes.txt.ferry-conflict.abababab-20260824-123456");
        assert_eq!(device_short(&dev), "abababab");
    }

    #[test]
    fn decomposed_names_normalize_to_one_conflict_name() {
        let dev = [0xCD; 32];
        let decomposed = conflict_display_name("rapport-anne\u{301}e.md", &dev, 1_787_574_896);
        let composed = conflict_display_name("rapport-ann\u{e9}e.md", &dev, 1_787_574_896);
        assert_eq!(decomposed, composed);
        assert_eq!(
            composed,
            "rapport-ann\u{e9}e.md.ferry-conflict.cdcdcdcd-20260824-123456"
        );
    }

    #[test]
    fn collisions_append_counters_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("f.txt.ferry-conflict.aa-20000101-000000"),
            b"x",
        )
        .unwrap();

        let d1 = unique_conflict_dest(dir.path(), &[], "f.txt.ferry-conflict.aa-20000101-000000")
            .unwrap();
        assert_eq!(
            d1.file_name().unwrap(),
            "f.txt.ferry-conflict.aa-20000101-000000-2"
        );

        std::fs::write(&d1, b"y").unwrap();
        let d2 = unique_conflict_dest(dir.path(), &[], "f.txt.ferry-conflict.aa-20000101-000000")
            .unwrap();
        assert_eq!(
            d2.file_name().unwrap(),
            "f.txt.ferry-conflict.aa-20000101-000000-3"
        );
    }

    #[test]
    fn dest_lands_next_to_the_original_inside_nested_parents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let d = unique_conflict_dest(
            dir.path(),
            &["a".to_string(), "b".to_string()],
            "f.ferry-conflict.cc-20000101-000000",
        )
        .unwrap();
        assert!(d.starts_with(dir.path().join("a/b")));
        assert!(!d.exists());
    }
}
