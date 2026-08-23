//! Symlink policy (SPEC guardrail: "sync as links where safe, refuse
//! dangerous cases loudly").
//!
//! Decision rule, applied identically at scan and materialize:
//!
//! - **Relative target that stays inside the folder root** → sync as a link.
//!   The link means the same thing on every device because it is resolved
//!   against its own location inside the synced tree. Chains of such links
//!   (`a -> b -> c -> file`) are fine: every hop is internal.
//! - **Absolute target** (`/etc/passwd`, `C:\Windows`, `\Users`, UNC) →
//!   REFUSE. An absolute path names a location OUTSIDE the folder; on another
//!   device it would silently point somewhere else entirely (or nowhere).
//! - **Relative target escaping the root** (`../../x`) → REFUSE. Same
//!   problem, lexical form: resolution leaves the folder boundary, so the
//!   link's meaning is host-specific.
//!
//! Refusals are loud and actionable: they carry the stored path, the raw
//! target, and the fix ("retarget inside the folder"). Nothing is silently
//! dropped or rewritten into something else.
//!
//! Windows directory links: creating ANY symlink on Windows needs developer
//! mode or admin privilege (research/landscape.md); junctions are the
//! legacy dir-link mechanism. Policy: refuse directory links on Windows at
//! materialize/apply time unless the documented escape hatch
//! `FERRY_ALLOW_WINDOWS_DIR_LINKS=1` is set in the environment (developer
//! mode documented as a requirement). The knob lives behind
//! [`allow_windows_dir_links`] so the gate is testable everywhere.

/// Why a symlink was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRefusal {
    /// Target is absolute (POSIX root, drive letter, backslash root, or UNC).
    AbsoluteTarget,
    /// Relative target resolves outside the folder root.
    EscapesRoot,
}

impl std::fmt::Display for LinkRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkRefusal::AbsoluteTarget => write!(
                f,
                "symlink target is absolute and would point outside the \
                 synced folder on other devices; retarget it relatively \
                 inside the folder"
            ),
            LinkRefusal::EscapesRoot => write!(
                f,
                "symlink target escapes the synced folder via '..'; \
                 retarget it to a path inside the folder"
            ),
        }
    }
}

/// What scan/materialize should do with one symlink entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkDecision {
    /// Store/restore as a real symlink; the target stays inside the root.
    SyncAsLink,
    /// Refuse loudly with this reason. The rest of the tree proceeds.
    Refuse(LinkRefusal),
}

/// Classify one symlink. `depth` is the number of components between the
/// folder root and the DIRECTORY containing the link (`0` = the link sits in
/// the root). `target` is the raw stored/read target string, verbatim.
///
/// Resolution is lexical over `/` AND `\` separators (links created on
/// Windows use backslashes), treating `.` as no-op and `..` as one level up;
/// ascending above depth 0 refuses.
pub fn classify_link(depth: usize, target: &str) -> LinkDecision {
    // Absolute forms across platforms.
    if target.starts_with('/') || target.starts_with('\\') {
        return LinkDecision::Refuse(LinkRefusal::AbsoluteTarget);
    }
    let b = target.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        // Drive-absolute ("C:\x") or drive-relative ("C:x"): both name a
        // location outside the folder by construction.
        return LinkDecision::Refuse(LinkRefusal::AbsoluteTarget);
    }

    let mut cur: i64 = depth as i64;
    for comp in target.split(['/', '\\']) {
        match comp {
            "" | "." => {}
            ".." => {
                cur -= 1;
                if cur < 0 {
                    return LinkDecision::Refuse(LinkRefusal::EscapesRoot);
                }
            }
            _ => cur += 1,
        }
    }
    LinkDecision::SyncAsLink
}

/// Windows-only escape hatch for directory symlinks/junctions (creation
/// requires developer mode or admin). Default OFF. Documented, deliberate,
/// env-gated: set `FERRY_ALLOW_WINDOWS_DIR_LINKS=1` to permit restoring
/// directory links on a Windows endpoint whose operator opted in.
///
/// On non-Windows hosts this always reports false — the knob simply does not
/// apply there (file symlinks need no privilege).
pub fn allow_windows_dir_links() -> bool {
    #[cfg(windows)]
    {
        std::env::var_os("FERRY_ALLOW_WINDOWS_DIR_LINKS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(depth: usize, target: &str) -> bool {
        classify_link(depth, target) == LinkDecision::SyncAsLink
    }

    #[test]
    fn internal_relative_links_sync_as_links() {
        assert!(sync(0, "docs"));
        assert!(sync(0, "docs/guide.md"));
        assert!(sync(2, "../../shared/asset.png"));
        assert!(sync(1, "../sibling.txt"));
        assert!(sync(0, "./here.txt"));
        // Chain hop: still relative and internal from deeper down.
        assert!(sync(3, "../../../root-file"));
        // Backslash-separated targets created on Windows hosts.
        assert!(sync(1, "..\\other\\file.txt"));
        assert!(sync(0, "dir\\nested"));
    }

    #[test]
    fn absolute_targets_are_refused_on_every_spelling() {
        for t in ["/etc/passwd", "\\Windows", "\\\\server\\share\\x", "C:\\Windows", "c:x"] {
            assert_eq!(
                classify_link(0, t),
                LinkDecision::Refuse(LinkRefusal::AbsoluteTarget),
                "{t}"
            );
        }
    }

    #[test]
    fn escaping_relative_targets_are_refused_exactly_at_the_boundary() {
        // From depth 1, one .. lands AT the root: fine. Two escape.
        assert!(sync(1, "../in-root"));
        assert_eq!(
            classify_link(1, "../../out"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );
        // From the root itself, any .. escapes immediately.
        assert_eq!(
            classify_link(0, ".."),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );
        // Descend then ascend past the origin: refused.
        assert_eq!(
            classify_link(0, "a/b/../../../out"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );
        // Descend then return exactly: allowed.
        assert!(sync(0, "a/../b"));
        // Escape attempt hidden after empty components: still caught.
        assert_eq!(
            classify_link(0, "./../evil"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );
    }

    #[test]
    fn windows_dir_link_gate_defaults_off_everywhere() {
        // No test may leak the env var; default must be off on all platforms.
        // (CI never sets FERRY_ALLOW_WINDOWS_DIR_LINKS.)
        assert!(!allow_windows_dir_links());
    }
}
