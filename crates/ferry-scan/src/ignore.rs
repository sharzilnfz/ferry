//! Ignore hooks consulted during walks and event filtering. T-011 ships the
//! real gitignore-syntax policy; this crate only defines the seam and a
//! permissive default, so ignore semantics can never leak into scan
//! internals.

/// Which side of gitignore's dir/file duality a queried path sits on.
///
/// Consulted paths always know their kind at the seam (T-12): the walker
/// stats each child immediately before consulting, sweeps read the kind off
/// cache payloads or their own stat, and ancestor components are
/// necessarily directories. Only raw watcher events lack it; the engine
/// resolves those itself without a disk access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// Plain file. Symlinks count as files here, matching both gitignore
    /// semantics and how links are stored in manifests.
    File,
    /// Directory.
    Dir,
}

/// Decides whether a relative path is excluded from scans and watches.
///
/// `rel` is the NFC-normalized component vector below the watched root
/// (root itself is `[]`); `kind` interprets the FINAL component. The engine
/// consults it:
///
/// - during directory listing (a dir returning `true` is pruned whole —
///   nothing under it is walked, hashed, or watched),
/// - when filtering watcher events (changes under an ignored dir are
///   dropped before they can dirty anything).
///
/// Implementations must be cheap and side-effect free; they run on walker
/// threads with the store lock idle.
pub trait IgnorePolicy: Send + Sync {
    fn ignored(&self, rel: &[String], kind: EntryKind) -> bool;
}

/// Default policy: nothing is ignored. Structural exclusion of the store
/// directory (`.ferry`) does NOT go through this trait; see crate docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoIgnores;

impl IgnorePolicy for NoIgnores {
    fn ignored(&self, _rel: &[String], _kind: EntryKind) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ignores_ignores_nothing() {
        for kind in [EntryKind::File, EntryKind::Dir] {
            assert!(!NoIgnores.ignored(&[], kind));
            assert!(!NoIgnores.ignored(&["node_modules".to_string()], kind));
            assert!(!NoIgnores.ignored(
                &[
                    "deep".to_string(),
                    "nesting".to_string(),
                    "x.txt".to_string()
                ],
                kind
            ));
        }
    }

    #[test]
    fn custom_policy_is_consulted_verbatim() {
        struct SkipNodeModules;
        impl IgnorePolicy for SkipNodeModules {
            fn ignored(&self, rel: &[String], _kind: EntryKind) -> bool {
                rel.first().map(std::string::String::as_str) == Some("node_modules")
            }
        }
        assert!(SkipNodeModules.ignored(&["node_modules".to_string()], EntryKind::Dir));
        assert!(SkipNodeModules.ignored(
            &["node_modules".to_string(), "a".to_string()],
            EntryKind::File
        ));
        assert!(!SkipNodeModules.ignored(&["src".to_string()], EntryKind::File));
    }
}
