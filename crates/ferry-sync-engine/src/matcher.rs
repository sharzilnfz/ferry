

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::pin_error::PinError;

#[derive(Clone, Debug)]
pub struct PathMatcher {
    
    gi: Option<Gitignore>,
    patterns: Vec<String>,
    match_all: bool,
}

impl PathMatcher {
    pub fn new(patterns: &[String]) -> Result<Self, PinError> {
        if patterns.iter().any(|p| p == "*") {
            return Ok(PathMatcher {
                gi: None,
                patterns: patterns.to_vec(),
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
            patterns: patterns.to_vec(),
            match_all: false,
        })
    }

    
    pub fn matches(&self, rel: &[String]) -> bool {
        if self.match_all {
            return true;
        }
        let Some(gi) = &self.gi else {
            return false;
        };
        let joined = rel.join("/");
        let path = std::path::Path::new(&joined);
        if matches!(
            gi.matched_path_or_any_parents(path, false),
            ignore::Match::Ignore(_)
        ) {
            return true;
        }
        self.patterns.iter().any(|pat| {
            let clean_pat = pat.trim_start_matches('/');
            clean_pat.starts_with(&format!("{joined}/"))
        })
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
    fn invalid_glob_is_loud_at_construction() {
        let err = PathMatcher::new(&["[z-a]".to_string()]).unwrap_err();
        assert!(matches!(err, PinError::BadPattern { .. }), "{err}");
    }
}
