





















use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;












pub fn fold_key(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    
    
    nfc.to_lowercase().replace('ς', "σ")
}


fn canonical(name: &str) -> String {
    name.nfc().collect()
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseConflict {
    
    pub key: String,
    
    pub first: String,
    
    pub second: String,
}

impl std::fmt::Display for CaseConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} and {:?} differ only by case and cannot coexist on this \
             filesystem; rename one of them",
            self.first, self.second
        )
    }
}



#[derive(Clone, Debug, Default)]
pub struct CaseFoldIndex {
    
    canonical: HashMap<String, String>,
}

impl CaseFoldIndex {
    pub fn new() -> Self {
        Self::default()
    }

    
    
    
    
    
    pub fn insert(&mut self, name: &str) -> Result<(), CaseConflict> {
        let key = fold_key(name);
        let canon = canonical(name);
        match self.canonical.get(&key) {
            Some(first) if *first != canon => Err(CaseConflict {
                key,
                first: first.clone(),
                second: canon,
            }),
            _ => {
                self.canonical.entry(key).or_insert(canon);
                Ok(())
            }
        }
    }
}



pub fn find_case_conflict(names: &[&str]) -> Option<CaseConflict> {
    let mut idx = CaseFoldIndex::new();
    for n in names {
        if let Err(c) = idx.insert(n) {
            return Some(c);
        }
    }
    None
}










pub fn host_folds_case() -> bool {
    cfg!(windows) || cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_key_ascii_and_unicode_cases_collapse() {
        assert_eq!(fold_key("README.md"), fold_key("readme.md"));
        assert_eq!(fold_key("ÄPFEL"), fold_key("äpfel"));
        
        
        assert_eq!(fold_key("ΟΔΟΣ"), fold_key("οδο\u{3c2}"));
        
        assert_eq!(fold_key("ПРОЕКТ"), fold_key("проект"));
        
        assert_eq!(fold_key("\u{212A}m"), fold_key("km"));
        
        
        assert_ne!(fold_key("\u{130}"), fold_key("i"));
    }

    #[test]
    fn fold_key_normalizes_decomposed_spellings_first() {
        
        assert_eq!(fold_key("caf\u{e9}.md"), fold_key("cafe\u{301}.md"));
    }

    #[test]
    fn index_reports_both_spellings_on_conflict() {
        let mut idx = CaseFoldIndex::new();
        idx.insert("Notes.txt").unwrap();
        idx.insert("other").unwrap();
        let err = idx.insert("NOTES.txt").unwrap_err();
        assert_eq!(err.first, "Notes.txt");
        assert_eq!(err.second, "NOTES.txt");
        assert_eq!(err.key, "notes.txt");
        
        
        idx.insert("Notes.txt").unwrap();
    }

    #[test]
    fn index_treats_nfc_equal_pair_as_one_name() {
        let mut idx = CaseFoldIndex::new();
        
        
        idx.insert("rapport-anne\u{301}e.md").unwrap();
        idx.insert("rapport-ann\u{e9}e.md").unwrap();

        
        idx.insert("RAPPORT-ANN\u{e9}E.MD").unwrap_err();
    }

    #[test]
    fn find_case_conflict_returns_first_pair_in_order() {
        assert!(find_case_conflict(&["a", "b", "c"]).is_none());
        let c = find_case_conflict(&["Alpha", "beta", "ALPHA"]).unwrap();
        assert_eq!((c.first.as_str(), c.second.as_str()), ("Alpha", "ALPHA"));
        let c2 = find_case_conflict(&["x", "y", "X", "Y"]).unwrap();
        assert_eq!((c2.first.as_str(), c2.second.as_str()), ("x", "X"));
        assert!(find_case_conflict(&[]).is_none());
    }

    #[test]
    fn empty_and_identical_names_are_handled() {
        let mut idx = CaseFoldIndex::new();
        idx.insert("").unwrap();
        assert!(idx.insert("").is_ok(), "same spelling twice is fine");
    }
}
