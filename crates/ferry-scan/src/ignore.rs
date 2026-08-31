











#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    
    
    File,
    
    Dir,
}














pub trait IgnorePolicy: Send + Sync {
    fn ignored(&self, rel: &[String], kind: EntryKind) -> bool;
}



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
