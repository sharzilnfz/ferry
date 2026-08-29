//! Compiled pin-path globs.
//!
//! `ferry pin start --paths 'src/**'` scopes the hold with gitignore-style
//! syntax, compiled by the SAME engine as `ferry.ignore` (the `ignore`
//! crate) so semantics match the rules users already know: `src/**`,
//! `docs/*`, bare directory names reach their children, last pattern wins
//! so `!` negation works too (supported by the engine; not required by the
//! ticket and not exercised as a scenario).
//!
//! The literal pattern `*` is special-cased to match EVERY path — the
//! ticket's `paths` glob-list-or-star whole-folder pin — because plain
//! gitignore `*` would not cross directory separators cleanly for every
//! shape of input.
//!
//! Invalid globs are refused loudly at `pin start` time ([`PathMatcher::
//! new`] errors), so a stored pin always compiles.

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::PinError;

#[derive(Clone, Debug)]
pub struct PathMatcher {
    /// Compiled matcher; `None` only for the `["*"]` match-all shortcut.
    gi: Option<Gitignore>,
    match_all: bool,
}

impl PathMatcher {
    pub fn new(patterns: &[String]) -> Result<Self, PinError> {
        if patterns.iter().any(|p| p == "*") {
            return Ok(PathMatcher {
                gi: None,
                match_all: true,
            });
        }
        let mut builder = GitignoreBuilder::new("");
        for line in patterns {
            builder
                .add_line(None, line)
                .map_err(|e| PinError::BadPattern {
                    line: line.clone(),
                    reason: e.to_string(),
                })?;
        }
        let gi = builder.build().map_err(|e| PinError::BadPattern {
            line: patterns.join(", "),
            reason: e.to_string(),
        })?;
        Ok(PathMatcher {
            gi: Some(gi),
            match_all: false,
        })
    }

    /// Does this stored-path scope cover `rel` ('/'-joined components)?
    /// Directory-scoped patterns reach their descendants (a pinned
    /// directory pins everything beneath it).
    pub fn matches(&self, rel: &[String]) -> bool {
        if self.match_all {
            return true;
        }
        let Some(gi) = &self.gi else {
            return false;
        };
        let joined = rel.join("/");
        let path = std::path::Path::new(&joined);
        // matched_path_or_any_parents walks ancestors, so a bare `src`
        // pattern holds `src/anything/deep.rs` too.
        matches!(
            gi.matched_path_or_any_parents(path, false),
            ignore::Match::Ignore(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> Vec<String> {
        path.split('/').map(str::to_string).collect()
    }

    fn m(patterns: &[&str]) -> PathMatcher {
        PathMatcher::new(
            &patterns
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn star_matches_everything_including_deep_paths() {
        let all = m(&["*"]);
        assert!(all.matches(&rel("notes.txt")));
        assert!(all.matches(&rel("src/main.rs")));
        assert!(all.matches(&rel("a/b/c/d.txt")));
    }

    #[test]
    fn doublestar_scope_reaches_descendants_only() {
        let src = m(&["src/**"]);
        assert!(src.matches(&rel("src/main.rs")));
        assert!(src.matches(&rel("src/deep/mod.rs")));
        assert!(!src.matches(&rel("docs/readme.md")));
        assert!(!src.matches(&rel("srcx/main.rs")), "no prefix bleed");
    }

    #[test]
    fn bare_directory_pattern_holds_its_children() {
        let src = m(&["src"]);
        assert!(src.matches(&rel("src/main.rs")));
        assert!(src.matches(&rel("src/nested/a.rs")));
        assert!(!src.matches(&rel("other/a.rs")));
    }

    #[test]
    fn overlapping_globs_union() {
        let both = m(&["src/**", "src/gen/**"]);
        assert!(both.matches(&rel("src/gen/out.rs")));
        assert!(both.matches(&rel("src/main.rs")));
        assert!(!both.matches(&rel("lib/gen/out.rs")));
    }

    #[test]
    fn non_matching_paths_bypass_the_hold_entirely() {
        let scoped = m(&["src/**"]);
        assert!(!scoped.matches(&rel("README.md")));
        assert!(!scoped.matches(&rel("assets/logo.png")));
        assert!(!scoped.matches(&rel("docs/readme.md")));
    }

    #[test]
    fn single_exact_path_pins_only_that_path() {
        // Bare gitignore names match at ANY depth, so pinning one exact file
        // needs the leading slash. Assert both behaviors explicitly.
        let bare = m(&["Cargo.toml"]);
        assert!(bare.matches(&rel("Cargo.toml")));
        assert!(
            bare.matches(&rel("sub/Cargo.toml")),
            "bare names reach every level, like ferry.ignore"
        );

        let anchored = m(&["/Cargo.toml"]);
        assert!(anchored.matches(&rel("Cargo.toml")));
        assert!(!anchored.matches(&rel("sub/Cargo.toml")));
    }

    #[test]
    fn negation_last_match_wins_engine_supported() {
        // Documented behavior: the same last-match-wins rule as
        // ferry.ignore applies inside one pin's pattern list.
        let neg = m(&["*.log", "!keep.log"]);
        assert!(neg.matches(&rel("noise.log")));
        assert!(!neg.matches(&rel("keep.log")));
    }

    #[test]
    fn invalid_glob_is_loud_at_construction() {
        let err = PathMatcher::new(&["[z-a]".to_string()]).unwrap_err();
        assert!(matches!(err, PinError::BadPattern { .. }), "{err}");
    }
}
