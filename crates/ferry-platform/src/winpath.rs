//! Windows long-path handling via `\\?\` extended-length prefixes.
//!
//! Background (research/landscape.md, Microsoft docs): classic Win32 paths
//! cap at MAX_PATH = 260 chars. The registry `LongPathsEnabled` value and a
//! `longPathAware` application manifest lift the cap, but a sync tool can
//! control neither on an arbitrary host — trees routinely exceed 260 chars
//! under nested project directories. The mechanical fix that works
//! regardless of host opt-in is the extended-length prefix: pass
//! `\\?\C:\very\long\path` (or `\\?\UNC\server\share\...` for UNC paths) to
//! skip normalization and the length check entirely.
//!
//! Rules implemented here (all pure, unit-tested on every platform):
//!
//! - Only Windows-shaped ABSOLUTE paths are touched: drive paths (`C:\...`)
//!   and UNC paths (`\\server\share\...`). Relative paths and POSIX paths
//!   are returned unchanged, so callers can apply this unconditionally.
//! - Already-prefixed paths are returned unchanged (idempotent).
//! - Forward slashes are normalized to backslashes inside prefixed paths:
//!   the `\\?\` form disables Win32's separator normalization.
//! - Per-component limits (~255 UTF-16 units) are NOT lifted by the prefix;
//!   NTFS simply cannot store longer names. Those failures surface as loud
//!   IO errors carrying the path.

use std::path::Path;

/// Classic Win32 path limit. A path at or beyond this length needs the
/// prefix unless the host opted in — which we cannot assume.
pub const MAX_PATH: usize = 260;

const EXTENDED_PREFIX: &str = "\\\\?\\";
const EXTENDED_UNC_PREFIX: &str = "\\\\?\\UNC\\";

/// Is this path already in `\\?\` / `\\?\UNC\` form?
pub fn is_extended_length(p: &Path) -> bool {
    let Some(s) = p.to_str() else {
        return false;
    };
    s.starts_with(EXTENDED_PREFIX)
}

/// Does this path need the extended-length prefix? True exactly when it is
/// a Windows-shaped absolute path, not yet prefixed, and its length meets or
/// exceeds [`MAX_PATH`] ("deep nesting" included: many short components sum
///ming past the cap is the common case).
pub fn needs_extended_length(p: &Path) -> bool {
    match windows_shape(p) {
        Some(_) => p
            .to_str()
            .map(|s| s.chars().count() >= MAX_PATH)
            .unwrap_or(false),
        None => false,
    }
}

/// Which extended-length shape does `p` have?
/// `Some(false)` = plain drive absolute; `Some(true)` = UNC.
fn windows_shape(p: &Path) -> Option<bool> {
    let s = p.to_str()?;
    if s.starts_with(EXTENDED_PREFIX) {
        return None; // already extended: never needs another prefix
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\\' && bytes[1] == b'\\' {
        return Some(true); // UNC \\server\share\...
    }
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        // Drive paths must be absolute (C:\...) to take the prefix; "C:x"
        // is drive-relative and must not be mangled.
        let absolute = bytes.len() >= 3 && (bytes[2] == b'\\' || bytes[2] == b'/');
        return if absolute { Some(false) } else { None };
    }
    None
}

/// Apply the `\\?\` prefix when the path is a long-enough Windows-shaped
/// absolute path; otherwise return the input unchanged. Idempotent.
///
/// On POSIX hosts this is always the identity function (POSIX absolute paths
/// start with `/`, which is not a Windows shape).
pub fn extend_path(p: &Path) -> std::path::PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None => return p.to_path_buf(), // non-UTF-8: leave for IO error reporting
    };
    let unc = match windows_shape(p) {
        Some(unc) => unc,
        None => return p.to_path_buf(),
    };
    if s.chars().count() < MAX_PATH {
        return p.to_path_buf();
    }
    if unc {
        // \\server\share\x -> \\?\UNC\server\share\x
        let body = s.trim_start_matches('\\').replace('/', "\\");
        format!("{EXTENDED_UNC_PREFIX}{body}").into()
    } else {
        let body = s.replace('/', "\\");
        format!("{EXTENDED_PREFIX}{body}").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn win(components: &[&str]) -> PathBuf {
        // Build a windows-shaped path portably from literal components.
        let joined = components.join("\\");
        PathBuf::from(joined)
    }

    #[test]
    fn const_max_path_is_the_documented_260() {
        assert_eq!(MAX_PATH, 260);
    }

    #[test]
    fn short_paths_pass_through_untouched() {
        assert_eq!(extend_path(Path::new(r"C:\Users\a\b.txt")), Path::new(r"C:\Users\a\b.txt"));
        assert!(!needs_extended_length(&win(&[r"C:\short", "file"])));
    }

    #[test]
    fn boundary_at_260_chars_gets_prefix_259_does_not() {
        // "C:\" + 256 chars = 259 total: under the cap, untouched.
        let stem = "d".repeat(256);
        let short = format!(r"C:\{stem}");
        assert_eq!(short.chars().count(), 259);
        assert_eq!(extend_path(Path::new(&short)), Path::new(&short));

        // One more char = 260 total: prefixed.
        let long = format!(r"C:\{stem}x");
        assert_eq!(long.chars().count(), 260);
        let got = extend_path(Path::new(&long));
        assert_eq!(got, PathBuf::from(format!(r"\\?\C:\{stem}x")));
        assert!(needs_extended_length(Path::new(&long)));
        // Idempotent: extending an extended path changes nothing.
        assert_eq!(extend_path(&got), got);
        assert!(!needs_extended_length(&got));
    }

    #[test]
    fn deep_nesting_past_the_cap_is_prefixed_and_normalized() {
        let mut parts: Vec<String> = vec![r"C:\work".to_string()];
        for i in 0..12 {
            parts.push(format!("level-{i:02}-directory-component"));
        }
        parts.push("leaf.bin".to_string());
        let parts_ref: Vec<&str> = parts.iter().map(String::as_str).collect();
        let p = win(&parts_ref);
        assert!(p.to_string_lossy().chars().count() > 260);

        let got = extend_path(&p);
        let s = got.to_string_lossy();
        assert!(s.starts_with("\\\\?\\C:\\work\\"), "{s}");
        assert!(!s.contains('/'), "separators normalized: {s}");
    }

    #[test]
    fn unc_paths_become_unc_extended_form() {
        let mut s = String::from(r"\\server\share");
        while s.chars().count() < 280 {
            s.push_str("\\nested");
        }
        let got = extend_path(Path::new(&s));
        let expect = format!("\\\\?\\UNC\\{}", s.trim_start_matches('\\').replace('/', "\\"));
        assert_eq!(got, PathBuf::from(expect));
    }

    #[test]
    fn relative_posix_and_drive_relative_paths_are_never_touched() {
        let rel = "a/b/c";
        assert_eq!(extend_path(Path::new(rel)), Path::new(rel));

        let posix_long = format!("/{}", "d".repeat(400));
        assert_eq!(extend_path(Path::new(&posix_long)), Path::new(&posix_long));
        assert!(!needs_extended_length(Path::new(&posix_long)));

        // Drive-relative C:x is not absolute; prefixing it would corrupt it.
        let drive_rel = format!("C:{}", "d".repeat(400));
        assert_eq!(extend_path(Path::new(&drive_rel)), Path::new(&drive_rel));
    }

    #[test]
    fn already_prefixed_paths_are_recognized() {
        let pre = Path::new(r"\\?\C:\anything\even\short");
        assert!(is_extended_length(pre));
        assert_eq!(extend_path(pre), pre);
        assert!(!needs_extended_length(pre));

        let pre_unc = Path::new(r"\\?\UNC\server\share\x");
        assert!(is_extended_length(pre_unc));
        assert_eq!(extend_path(pre_unc), pre_unc);

        assert!(!is_extended_length(Path::new(r"C:\plain")));
    }

    #[test]
    fn non_utf8_paths_pass_through_without_panic() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let weird = std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe").to_owned();
            assert_eq!(extend_path(weird.as_ref()), weird);
        }
    }
}
