//! Temp-name conventions and stale-temp sweeping (T-005).
//!
//! Borrowed from Syncthing BEP via `research/landscape.md`: never write a
//! destination in place; write a sibling temp file in the DESTINATION
//! directory (same filesystem, so the final rename is atomic) and rename it
//! over the destination.
//!
//! Names:
//!
//! - macOS/Linux: `.ferry.<name>.tmp[.<entropy>]` inside the destination's
//!   directory. Leading-dot names are conventional and mostly invisible.
//! - Windows-style: `~ferry~<name>.tmp[.<entropy>]`, because leading-dot
//!   names are awkward on Windows. Selected by `cfg!(windows)` at runtime,
//!   but both styles are pure functions here and unit-tested on every
//!   platform.
//! - Overflow: if prefix + name + suffix would push the component past the
//!   conservative length limit, a hash-substituted short name
//!   `<prefix><16 hex of BLAKE3(rel path)>.tmp` replaces it (Syncthing does
//!   the same when prefix+extension would overflow path limits).
//!
//! The optional `.<entropy>` tail is 8 lowercase hex chars so two writers
//! can never collide on one temp name and stale-temp detection stays
//! unambiguous.
//!
//! Reserved names: any file matching [`is_temp_name`] in a synced tree is
//! materializer territory. A user file literally named `.ferry.notes.tmp`
//! would be swept eventually — same reserved-word tradeoff Syncthing makes.
//!
//! Stale temps: a crash leaves orphaned temps behind. Like Syncthing (which
//! keeps them about a day to allow resuming), we keep them for
//! [`DEFAULT_STALE_TEMP_AGE_SECS`] and sweep older ones at startup via
//! [`sweep_stale_temps`]. Keeping them briefly also preserves the written
//! bytes, so an interrupted large transfer can be resumed from the temp
//! rather than refetched.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::{io_at, MaterializeError};

/// Suffix shared by every temp name.
pub const TEMP_SUFFIX: &str = ".tmp";

/// Length of the random entropy tail (hex chars).
pub const ENTROPY_HEX_LEN: usize = 8;

/// Conservative single-component length cap. Stays under `NAME_MAX` (255) on
/// Linux/macOS with room for the prefix/suffix/entropy overhead, and under
/// typical Windows per-component limits.
const NAME_LEN_LIMIT: usize = 200;

/// How long orphaned temp files are kept before sweeping (Syncthing keeps
/// them up to a day for resume). Seconds-scale values work fine in tests.
pub const DEFAULT_STALE_TEMP_AGE_SECS: u64 = 24 * 60 * 60;

/// Which spelling of the temp prefix to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempStyle {
    /// `.ferry.<name>.tmp` (macOS/Linux).
    Dot,
    /// `~ferry~<name>.tmp` (Windows-style; leading dots are awkward there).
    Windows,
}

impl TempStyle {
    /// The style selected for this host (`cfg!(windows)`).
    pub fn current() -> Self {
        if cfg!(windows) {
            TempStyle::Windows
        } else {
            TempStyle::Dot
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            TempStyle::Dot => ".ferry.",
            TempStyle::Windows => "~ferry~",
        }
    }
}

/// The plain mangled name for one destination file name (last component
/// only), optionally carrying an entropy tail.
pub fn temp_file_name(dest_name: &str, style: TempStyle, entropy: &str) -> String {
    debug_assert!(
        !dest_name.contains('/'),
        "temp_file_name takes the final component"
    );
    let mut s = String::with_capacity(
        style.prefix().len() + dest_name.len() + TEMP_SUFFIX.len() + entropy.len() + 1,
    );
    s.push_str(style.prefix());
    s.push_str(dest_name);
    s.push_str(TEMP_SUFFIX);
    if !entropy.is_empty() {
        s.push('.');
        s.push_str(entropy);
    }
    s
}

/// Hash-substituted temp name used when the plain form would overflow path
/// limits. Hashes the full relative path (not just the component) so two
/// long-named files in different directories cannot collide.
pub fn hashed_temp_file_name(rel_path: &str, style: TempStyle) -> String {
    let digest = blake3::hash(rel_path.as_bytes());
    format!(
        "{}{}{}",
        style.prefix(),
        &digest.to_hex()[..16],
        TEMP_SUFFIX
    )
}

/// Full convention: plain form unless it would overflow, then the hashed
/// form. `rel_path` is `/`-separated relative to the sync root; only its
/// last component appears in the plain form.
pub fn temp_name_for(rel_path: &str, style: TempStyle, entropy: &str) -> String {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let candidate = temp_file_name(name, style, entropy);
    if candidate.len() > NAME_LEN_LIMIT || name.is_empty() {
        hashed_temp_file_name(rel_path, style)
    } else {
        candidate
    }
}

/// Does this file name match either documented temp pattern?
///
/// Accepts `<prefix><anything>.tmp` and `<prefix><anything>.tmp.<8 hex>` for
/// BOTH styles regardless of host, so a tree synced cross-platform sweeps
/// cleanly everywhere.
pub fn is_temp_name(name: &str) -> bool {
    [TempStyle::Dot, TempStyle::Windows]
        .iter()
        .any(|&style| matches_style(name, style))
}

fn matches_style(name: &str, style: TempStyle) -> bool {
    let Some(rest) = name.strip_prefix(style.prefix()) else {
        return false;
    };
    // Plain form: some nonempty body then exactly ".tmp".
    if rest.len() > TEMP_SUFFIX.len() && rest.ends_with(TEMP_SUFFIX) {
        return true;
    }
    // Entropy form: ".tmp" followed by "." + exactly 8 hex chars.
    if let Some(pos) = rest.rfind(TEMP_SUFFIX) {
        if let Some(ent) = rest[pos + TEMP_SUFFIX.len()..].strip_prefix('.') {
            return ent.len() == ENTROPY_HEX_LEN && ent.bytes().all(|b| b.is_ascii_hexdigit());
        }
    }
    false
}

fn hex_entropy() -> String {
    use rand::Rng;
    let mut bytes = [0u8; ENTROPY_HEX_LEN / 2];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Draw a fresh entropy tail for one temp file.
pub fn fresh_entropy() -> String {
    hex_entropy()
}

/// Delete every temp-pattern file under `target_root` whose mtime is older
/// than `max_age`. Never follows symlinks while walking; removes temp
/// SYMLINKS too (a crash can orphan one mid rename). Returns the removed
/// paths, sorted.
///
/// Call once at startup before applying anything (documented startup
/// sequence); it is deliberately NOT implicit inside apply.
pub fn sweep_stale_temps(
    target_root: &Path,
    max_age: Duration,
) -> Result<Vec<PathBuf>, MaterializeError> {
    let cutoff =
        SystemTime::now()
            .checked_sub(max_age)
            .ok_or_else(|| MaterializeError::BadComponent {
                component: "max_age overflow".into(),
            })?;
    let mut removed = Vec::new();
    let mut stack = vec![target_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_at(&dir, e)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&path, e)),
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_temp_name(&name) {
                continue;
            }
            let modified = match std::fs::symlink_metadata(&path) {
                Ok(m) => m.modified(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&path, e)),
            };
            match modified {
                Ok(t) if t < cutoff => match std::fs::remove_file(&path) {
                    Ok(()) => removed.push(path),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_at(&path, e)),
                },
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_at(&path, e)),
            }
        }
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mangling_matches_documented_patterns_both_styles() {
        assert_eq!(
            temp_file_name("main.rs", TempStyle::Dot, ""),
            ".ferry.main.rs.tmp"
        );
        assert_eq!(
            temp_file_name("main.rs", TempStyle::Windows, ""),
            "~ferry~main.rs.tmp"
        );
        // With entropy tail.
        assert_eq!(
            temp_file_name("a.txt", TempStyle::Dot, "0123abcd"),
            ".ferry.a.txt.tmp.0123abcd"
        );
        // Unicode names pass through untouched apart from the mangling.
        assert_eq!(
            temp_file_name("café.txt", TempStyle::Dot, ""),
            ".ferry.café.txt.tmp"
        );
    }

    #[test]
    fn current_style_follows_host_cfg() {
        #[cfg(windows)]
        assert_eq!(TempStyle::current(), TempStyle::Windows);
        #[cfg(not(windows))]
        assert_eq!(TempStyle::current(), TempStyle::Dot);
    }

    #[test]
    fn long_names_fall_back_to_hash_substitution() {
        // 195-char component: even the empty-entropy plain form overflows
        // the limit, so the hash fallback is unconditional for this path.
        let long_rel = format!("{}/{}.rs", "d".repeat(80), "n".repeat(195));
        let plain = temp_file_name(long_rel.rsplit('/').next().unwrap(), TempStyle::Dot, "");
        assert!(plain.len() > NAME_LEN_LIMIT);

        let picked = temp_name_for(&long_rel, TempStyle::Dot, "00112233");
        assert!(picked.len() <= NAME_LEN_LIMIT, "{picked}");
        assert_eq!(picked, hashed_temp_file_name(&long_rel, TempStyle::Dot));
        // Deterministic for the same path (entropy is dropped by the hashed
        // form), distinct across paths and styles.
        assert_eq!(picked, temp_name_for(&long_rel, TempStyle::Dot, "x"));
        assert_ne!(
            hashed_temp_file_name(&long_rel, TempStyle::Dot),
            hashed_temp_file_name(&format!("{long_rel}2"), TempStyle::Dot)
        );
        assert_ne!(
            hashed_temp_file_name(&long_rel, TempStyle::Dot),
            hashed_temp_file_name(&long_rel, TempStyle::Windows)
        );
        assert!(is_temp_name(&picked));
    }

    #[test]
    fn is_temp_name_accepts_every_documented_form_and_nothing_else() {
        // Both styles, with and without entropy.
        assert!(is_temp_name(".ferry.x.tmp"));
        assert!(is_temp_name(".ferry.x.tmp.deadbeef"));
        assert!(is_temp_name("~ferry~x.tmp"));
        assert!(is_temp_name("~ferry~x.tmp.DEADBEEF"));
        // Real files stay untouched.
        assert!(!is_temp_name("x.tmp"));
        assert!(!is_temp_name(".ferry"));
        assert!(!is_temp_name(".ferryx.tmp")); // no separator dot after prefix
        assert!(!is_temp_name("~ferry~x.tmp.short")); // wrong entropy length
        assert!(!is_temp_name("~ferry~x.tmp.nothex!"));
        assert!(!is_temp_name(".ferry..tmp")); // empty body
                                               // An unlucky-but-legal user file with a single trailing char is not
                                               // an 8-hex entropy tail, so it is NOT ours.
        assert!(!is_temp_name(".ferry.notes.tmp.b"));
    }

    #[test]
    fn sweep_removes_only_aged_temps_and_leaves_live_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join(".ferry.old.tmp.1234abcd"), b"stale").unwrap();
        std::fs::write(root.join("~ferry~old2.tmp"), b"stale win-style").unwrap();
        std::fs::write(root.join(".ferry.fresh.tmp.aabbccdd"), b"fresh").unwrap();
        std::fs::write(root.join("real.txt"), b"user data").unwrap();
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("subdir/.ferry.deep.tmp"), b"stale deep").unwrap();

        // Backdate the three stale candidates.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        for name in [
            ".ferry.old.tmp.1234abcd",
            "~ferry~old2.tmp",
            "subdir/.ferry.deep.tmp",
        ] {
            let f = std::fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }

        let removed = sweep_stale_temps(root, Duration::from_mins(1)).unwrap();
        let names: Vec<String> = removed
            .iter()
            .map(|p| {
                // windows read_dir hands back backslash separators; the
                // repo's display convention is forward slashes.
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ".ferry.old.tmp.1234abcd",
                "subdir/.ferry.deep.tmp",
                "~ferry~old2.tmp"
            ]
        );
        assert!(!root.join(".ferry.old.tmp.1234abcd").exists());
        assert!(root.join(".ferry.fresh.tmp.aabbccdd").exists());
        assert!(root.join("real.txt").exists());

        // Default age constant documents the Syncthing-style day.
        assert_eq!(DEFAULT_STALE_TEMP_AGE_SECS, 86_400);
    }

    #[test]
    fn sweep_tolerates_vanishing_entries_mid_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".ferry.gone.tmp"), b"x").unwrap();
        // No panic when the entry disappears between listing and stat.
        let removed = sweep_stale_temps(dir.path(), Duration::from_hours(1)).unwrap();
        // Its mtime is now, so nothing should be removed anyway.
        assert!(removed.is_empty());
    }
}
