//! Conflict-file naming.
//!
//! Template (ADR-0004): `<stored path>.ferry-conflict.<device>-<ts>`.
//! Concrete rule, decided here and asserted by tests:
//!
//! - The device tag is the LOSING side's short id: first 8 hex chars of its
//!   32-byte device id. Syncthing's convention: the name records where the
//!   losing copy came from, which is what you want when triaging.
//! - `<ts>` is the loser entry's own mtime formatted `YYYYMMDD-HHMMSS` UTC,
//!   not a wall clock, so both devices derive the same expected name and
//!   tests are deterministic.
//! - No extension splitting; the marker is one suffix on the full stored
//!   path (`report.docx.ferry-conflict.ab12cd34-20260824-101500`).
//! - Collision rule on repeated conflicts: try `NAME`, then `NAME-2`,
//!   `NAME-3`, ... in that exact order; the first name absent from the live
//!   directory wins. Counters append at the end because the template has no
//!   extension to collapse into.

use std::path::{Path, PathBuf};

use ferry_store::format::hex;
use unicode_normalization::UnicodeNormalization;

/// First 8 lowercase hex chars of a device id.
pub fn device_short(device: &[u8; 32]) -> String {
    hex(device)[..8].to_string()
}

/// The deterministic first-choice conflict name for one loser copy.
///
/// The original name is defensively NFC-normalized: stored names are already
/// NFC by construction, but quarantine naming is the last line of display,
/// so a decomposed spelling can never leak into a conflict filename (and
/// then look like two files to users on folding hosts). NFC-equal input
/// therefore yields the identical conflict name everywhere (T-012 audit).
pub fn conflict_display_name(
    original_name: &str,
    loser_device: &[u8; 32],
    loser_mtime_sec: i64,
) -> String {
    let nfc: String = original_name.nfc().collect();
    format!(
        "{nfc}.ferry-conflict.{}-{}",
        device_short(loser_device),
        crate::timefmt::fmt_compact(loser_mtime_sec)
    )
}

/// Resolve the first free candidate — `NAME`, then `NAME-2`, `NAME-3`,
/// ... with an incrementing counter — inside the destination directory, and
/// return it as an absolute path. Does not create anything.
///
/// This probe is advisory only: another executor can claim the returned
/// name before the copy lands. The executor enforces real exclusivity at
/// landing time (see `execute::write_loser_copy`), regenerating counter
/// names until an exclusive placement succeeds.
///
/// `relative_parent` is the quarantined file's parent as stored components;
/// pass `&[]` for the root.
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
        // T-012 NFC audit: decomposed and precomposed spellings of the same
        // name MUST produce the identical quarantine filename — one name,
        // not two, regardless of which spelling the manifest carried.
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

        // First choice taken → NAME-2.
        let d1 = unique_conflict_dest(dir.path(), &[], "f.txt.ferry-conflict.aa-20000101-000000")
            .unwrap();
        assert_eq!(
            d1.file_name().unwrap(),
            "f.txt.ferry-conflict.aa-20000101-000000-2"
        );

        // Take -2 as well → -3, and so on.
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
