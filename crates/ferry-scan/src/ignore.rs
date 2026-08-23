//! Ignore hooks consulted during walks and event filtering. T-011 ships the
//! real gitignore-syntax policy; this crate only defines the seam and a
//! permissive default, so ignore semantics can never leak into scan
//! internals.

/// Decides whether a relative path is excluded from scans and watches.
///
/// `rel` is the NFC-normalized component vector below the watched root
/// (root itself is `[]`). The engine consults it:
///
/// - during directory listing (a dir returning `true` is pruned whole —
///   nothing under it is walked, hashed, or watched),
/// - when filtering watcher events (changes under an ignored dir are
///   dropped before they can dirty anything),
/// - when registering watches on new directories.
///
/// Implementations must be cheap and side-effect free; they run on walker
/// threads with the store lock idle.
pub trait IgnorePolicy: Send + Sync {
    fn ignored(&self, rel: &[String]) -> bool;
}

/// Default policy: nothing is ignored. Structural exclusion of the store
/// directory (`.ferry`) does NOT go through this trait; see crate docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoIgnores;

impl IgnorePolicy for NoIgnores {
    fn ignored(&self, _rel: &[String]) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ignores_ignores_nothing() {
        assert!(!NoIgnores.ignored(&[]));
        assert!(!NoIgnores.ignored(&["node_modules".to_string()]));
        assert!(!NoIgnores.ignored(&[
            "deep".to_string(),
            "nesting".to_string(),
            "x.txt".to_string()
        ]));
    }

    #[test]
    fn custom_policy_is_consulted_verbatim() {
        struct SkipNodeModules;
        impl IgnorePolicy for SkipNodeModules {
            fn ignored(&self, rel: &[String]) -> bool {
                rel.first().map(|s| s.as_str()) == Some("node_modules")
            }
        }
        assert!(SkipNodeModules.ignored(&["node_modules".to_string()]));
        assert!(SkipNodeModules.ignored(&["node_modules".to_string(), "a".to_string()]));
        assert!(!SkipNodeModules.ignored(&["src".to_string()]));
    }
}
