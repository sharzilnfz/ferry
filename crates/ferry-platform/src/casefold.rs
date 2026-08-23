//! Case-conflict detection: a per-folder case-folding index mapping each
//! name to its fold key, so two live entries that would collide on a
//! case-insensitive filesystem are caught at scan/materialize time and
//! refused loudly — never silently merged, never silently picked.
//!
//! Why this exists (research/landscape.md): Syncthing's fix for case-only
//! collisions (`lib/fs/casefs.go`) took five years and multiple rewrites,
//! with real data loss before 1.9.0. The lesson is to detect from day one.
//!
//! Fold rule: NFC-normalize, then Unicode `to_lowercase` (simple-fold
//! semantics, like Go's `unicode.SimpleFold` that Syncthing uses). This is
//! deliberately NOT full Unicode caseless folding (NFKD + CaseFold): ß stays
//! ß rather than becoming ss, matching prior art and keeping keys stable.
//! Locale tailoring (Turkish i) is out of scope; the default (non-locale)
//! lowercase table is used everywhere so all devices agree.
//!
//! Host policy: [`host_folds_case()`] reports whether THIS device's
//! filesystem folds case. On such hosts a fold collision between live
//! siblings is fatal at scan and at materialize. On case-sensitive hosts
//! (Linux) both entries legitimately coexist on disk, so scan allows them;
//! materialize still refuses when writing onto a folding host.

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

/// Fold one name to its case-insensitive comparison key. Input need not be
/// pre-normalized: decomposed and composed spellings produce one key.
/// Greek final sigma folds onto σ (matching Go's `unicode.SimpleFold`
/// orbit that Syncthing's casefs uses).
///
/// ```
/// assert_eq!(
///     ferry_platform::fold_key("caf\u{e9}.txt"),
///     ferry_platform::fold_key("cafe\u{301}.TXT"),
/// );
/// ```
pub fn fold_key(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    // Lowercase first (Rust maps word-final Σ to ς per SpecialCasing), then
    // collapse the ς↔σ orbit so both spellings share one key.
    nfc.to_lowercase().replace('ς', "σ")
}

/// Canonical storage spelling of a name: NFC.
fn canonical(name: &str) -> String {
    name.nfc().collect()
}

/// A detected collision: two distinct stored names sharing one fold key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseConflict {
    /// The shared fold key (diagnostics only).
    pub key: String,
    /// First spelling seen (NFC).
    pub first: String,
    /// Second spelling seen (NFC).
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

/// Per-folder index of names already accepted, keyed by fold key. Inserting
/// a name that folds onto an existing entry fails with both spellings named.
#[derive(Clone, Debug, Default)]
pub struct CaseFoldIndex {
    /// fold key -> first canonical (NFC) spelling inserted.
    canonical: HashMap<String, String>,
}

impl CaseFoldIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `name`. Returns Err([`CaseConflict`]) when an already-
    /// registered name folds to the same key. The first-inserted NFC
    /// spelling stays canonical for the lifetime of the folder listing.
    /// A spelling that is merely a different normalization of an existing
    /// entry (decomposed vs precomposed) IS the same name: Ok.
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

/// Convenience check over one directory listing: returns the FIRST
/// collision found (deterministic: first pair in input order).
pub fn find_case_conflict(names: &[&str]) -> Option<CaseConflict> {
    let mut idx = CaseFoldIndex::new();
    for n in names {
        if let Err(c) = idx.insert(n) {
            return Some(c);
        }
    }
    None
}

/// Does this host's standard filesystem fold case?
///
/// - Windows: NTFS/exFAT/FAT are case-insensitive by default (per-directory
///   sensitivity exists but is exotic). Always true.
/// - macOS: APFS and HFS+ are case-insensitive unless the volume was
///   explicitly formatted case-sensitive (rare, opt-in). True — the safe
///   default; a case-sensitive-APFS user syncing `README` + `readme` gets a
///   loud refusal instead of silent breakage on every mainstream peer.
/// - Linux/other unix: ext4/xfs/btrfs/zfs are case-sensitive. False.
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
        // Greek final sigma vs capital sigma.
        // Greek final sigma vs capital sigma (same SimpleFold orbit).
        assert_eq!(fold_key("ΟΔΟΣ"), fold_key("οδο\u{3c2}"));
        // Cyrillic.
        assert_eq!(fold_key("ПРОЕКТ"), fold_key("проект"));
        // Kelvin sign folds onto plain k (simple-fold behavior).
        assert_eq!(fold_key("\u{212A}m"), fold_key("km"));
        // Turkish dotted capital I lowercases to i + combining dot; NOT
        // equal to plain i (default table, no locale tailoring).
        assert_ne!(fold_key("\u{130}"), fold_key("i"));
    }

    #[test]
    fn fold_key_normalizes_decomposed_spellings_first() {
        // Precomposed vs decomposed é: ONE key, not two.
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
        // Inserting the identical spelling again is idempotent, not a
        // conflict (the caller may re-list the same entry).
        idx.insert("Notes.txt").unwrap();
    }

    #[test]
    fn index_treats_nfc_equal_pair_as_one_name() {
        let mut idx = CaseFoldIndex::new();
        // Decomposed "rapport-année" vs precomposed: same name after NFC,
        // therefore the SAME index entry — not two.
        idx.insert("rapport-anne\u{301}e.md").unwrap();
        idx.insert("rapport-ann\u{e9}e.md").unwrap();

        // But a case-only variant of the canonical name still conflicts.
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
