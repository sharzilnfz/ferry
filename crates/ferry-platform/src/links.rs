#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRefusal {
    AbsoluteTarget,

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkDecision {
    SyncAsLink,

    Refuse(LinkRefusal),
}

pub fn classify_link(depth: usize, target: &str) -> LinkDecision {
    if target.starts_with('/') || target.starts_with('\\') {
        return LinkDecision::Refuse(LinkRefusal::AbsoluteTarget);
    }
    let b = target.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
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

pub fn allow_windows_dir_links() -> bool {
    #[cfg(windows)]
    {
        std::env::var_os("FERRY_ALLOW_WINDOWS_DIR_LINKS").is_some_and(|v| v == "1")
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

        assert!(sync(3, "../../../root-file"));

        assert!(sync(1, "..\\other\\file.txt"));
        assert!(sync(0, "dir\\nested"));
    }

    #[test]
    fn absolute_targets_are_refused_on_every_spelling() {
        for t in [
            "/etc/passwd",
            "\\Windows",
            "\\\\server\\share\\x",
            "C:\\Windows",
            "c:x",
        ] {
            assert_eq!(
                classify_link(0, t),
                LinkDecision::Refuse(LinkRefusal::AbsoluteTarget),
                "{t}"
            );
        }
    }

    #[test]
    fn escaping_relative_targets_are_refused_exactly_at_the_boundary() {
        assert!(sync(1, "../in-root"));
        assert_eq!(
            classify_link(1, "../../out"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );

        assert_eq!(
            classify_link(0, ".."),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );

        assert_eq!(
            classify_link(0, "a/b/../../../out"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );

        assert!(sync(0, "a/../b"));

        assert_eq!(
            classify_link(0, "./../evil"),
            LinkDecision::Refuse(LinkRefusal::EscapesRoot)
        );
    }

    #[test]
    fn windows_dir_link_gate_defaults_off_everywhere() {
        assert!(!allow_windows_dir_links());
    }
}
