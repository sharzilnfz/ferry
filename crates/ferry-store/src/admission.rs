//! The per-entry scan admission gate (T-11): ONE implementation of the
//! rules that decide whether a single directory entry may enter a
//! manifest, shared verbatim by the from-scratch snapshot walker
//! ([`crate::snapshot::snapshot_dir`]) and the incremental pass builder
//! (ferry-scan's `walk::Walker`). "Incremental == from-scratch" holds by
//! construction because neither caller keeps its own copy of these rules
//! anymore.
//!
//! The rules, in the order the gate applies them:
//!
//! 1. **Name admissibility** ([`admit_name`]): the raw on-disk name must
//!    decode as UTF-8 and is NFC-normalized. Non-UTF-8 names are refused
//!    loudly ([`RefusalReason::NonUtf8Name`]); the ledger records the
//!    lossy rendering so the path is still identifiable.
//! 2. **Kind admissibility** ([`admit_kind`]), after the caller has run
//!    any walker-local filters (structural store-dir exclusion, ignore
//!    policy) that sit between name and kind:
//!    - reserved Windows device names are refused
//!      ([`RefusalReason::ReservedName`]); they can never materialize on
//!      a Windows endpoint,
//!    - symlink targets must decode as UTF-8 and pass
//!      [`ferry_platform::classify_link`] — relative internal targets
//!      sync as links, absolute or root-escaping targets are refused,
//!    - anything that is not a file, directory, or symlink (sockets,
//!      FIFOs, devices) has no manifest representation and is refused.
//!
//! What the gate deliberately does NOT do: chunk files, read mtimes or
//! exec bits, consult ignore rules, detect sibling collisions, or touch
//! the disk beyond what the caller already stat'ed. Sibling collision
//! detection lives in [`crate::snapshot::ensure_no_collisions`] (already
//! one implementation called by both walkers). Payload construction stays
//! caller-side because it legitimately differs (chunking vs cache reuse).
//!
//! Refusal reasons and their Display messages are owned by
//! [`crate::snapshot::RefusalReason`] and pass through untouched; tests
//! assert those strings exactly.

use std::ffi::OsStr;

use ferry_platform::{classify_link, is_reserved_device_name};
use unicode_normalization::UnicodeNormalization;

pub use crate::snapshot::RefusalReason;

/// Entry kind as observed by `lstat` (never following symlinks). The
/// caller maps `Metadata::file_type()` onto this; the gate never stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedKind {
    File,
    Dir,
    Symlink,
    /// Sockets, FIFOs, devices: no manifest representation exists.
    Other,
}

/// Why an entry was refused, plus the name to record in the refusal
/// ledger. The ledger path is always `<parent components> + display_name`;
/// only the reason semantics live here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: RefusalReason,
    /// What to append to the parent path when recording this refusal: the
    /// NFC component for every reason except [`RefusalReason::NonUtf8Name`],
    /// whose raw bytes have no valid component — there it is the lossy
    /// UTF-8 rendering, same as this module has always shown.
    pub display_name: String,
}

/// An admitted entry, reduced to what payload construction needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedEntry {
    /// NFC-normalized name under which the entry enters the tree node.
    pub component: String,
    pub kind: AdmittedKind,
}

/// The admitted kind, with link payloads resolved to their stored form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmittedKind {
    File,
    Dir,
    Symlink {
        /// Target as stored in the manifest: valid UTF-8, policy-cleared,
        /// verbatim apart from that (same byte string `read_link` yielded).
        target: String,
    },
}

/// Everything the one-shot gate needs about one directory entry. All of it
/// comes from `readdir` plus the single `lstat` the caller already spent;
/// the caller never applies per-entry policy itself.
#[derive(Clone, Debug)]
pub struct EntryFacts<'a> {
    /// Raw on-disk name (parent-relative), exactly as readdir yielded it.
    pub raw_name: &'a OsStr,
    pub kind: ObservedKind,
    /// Raw link target for symlinks, `None` otherwise. May be non-UTF-8;
    /// that is a refusal the gate decides, not an error.
    pub link_target: Option<&'a OsStr>,
    /// Number of directory components between the folder root and this
    /// entry's PARENT (`0` = the entry sits in the root listing). Drives
    /// the `..`-escape analysis for relative link targets.
    pub parent_depth: usize,
}

// ---- Phase 1: name admission ------------------------------------------

/// Name half of the gate: UTF-8 + NFC. Walkers that must interpose their
/// own filters between name and kind (the incremental walker's structural
/// store-directory exclusion and ignore policy) call this first; the
/// from-scratch walker uses the composed [`admit`] instead.
pub fn admit_name(raw: &OsStr) -> Result<String, Refusal> {
    match std::str::from_utf8(raw.as_encoded_bytes()) {
        Ok(s) => Ok(s.nfc().collect::<String>()),
        // A name with no valid UTF-8 form cannot become a component; the
        // ledger keeps the lossy rendering so humans can still find it.
        Err(_) => Err(Refusal {
            reason: RefusalReason::NonUtf8Name,
            display_name: String::from_utf8_lossy(raw.as_encoded_bytes()).into_owned(),
        }),
    }
}

// ---- Phase 2: kind admission ------------------------------------------

/// Kind half of the gate: reserved names, symlink policy, representable
/// file types. Takes the already-normalized component from [`admit_name`].
pub fn admit_kind(
    component: String,
    kind: ObservedKind,
    link_target: Option<&OsStr>,
    parent_depth: usize,
) -> Result<AdmittedEntry, Refusal> {
    let refuse = |reason| -> Result<AdmittedEntry, Refusal> {
        Err(Refusal {
            reason,
            display_name: component.clone(),
        })
    };

    // Reserved Windows device names can never be materialized on a
    // Windows endpoint; refuse loudly at the source (T-012).
    if is_reserved_device_name(&component) {
        return refuse(RefusalReason::ReservedName);
    }

    let admitted = match kind {
        ObservedKind::File => AdmittedKind::File,
        ObservedKind::Dir => AdmittedKind::Dir,
        ObservedKind::Other => {
            // Sockets, FIFOs, devices: no manifest representation exists.
            return refuse(RefusalReason::UnknownFileType);
        }
        ObservedKind::Symlink => {
            let Some(raw) = link_target else {
                // Callers only construct Symlink facts with a target; a
                // missing one is treated like an undecodable target rather
                // than a panic.
                return refuse(RefusalReason::NonUtf8SymlinkTarget);
            };
            let Some(t) = raw.to_str() else {
                return refuse(RefusalReason::NonUtf8SymlinkTarget);
            };
            // T-012 symlink policy: relative internal targets sync as
            // links; absolute or root-escaping targets are refused loudly.
            match classify_link(parent_depth, t) {
                ferry_platform::LinkDecision::SyncAsLink => AdmittedKind::Symlink {
                    target: t.to_owned(),
                },
                ferry_platform::LinkDecision::Refuse(reason) => {
                    let r = match reason {
                        ferry_platform::LinkRefusal::AbsoluteTarget => {
                            RefusalReason::AbsoluteSymlinkTarget
                        }
                        ferry_platform::LinkRefusal::EscapesRoot => {
                            RefusalReason::EscapingSymlinkTarget
                        }
                    };
                    return refuse(r);
                }
            }
        }
    };
    Ok(AdmittedEntry {
        component,
        kind: admitted,
    })
}

// ---- One-shot gate -----------------------------------------------------

/// The whole admission decision for one entry, for walkers with no filters
/// between name and kind (the from-scratch snapshot walker).
pub fn admit(facts: EntryFacts<'_>) -> Result<AdmittedEntry, Refusal> {
    let component = admit_name(facts.raw_name)?;
    admit_kind(component, facts.kind, facts.link_target, facts.parent_depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[cfg(unix)]
    fn os(bytes: &[u8]) -> &OsStr {
        use std::os::unix::ffi::OsStrExt;
        OsStr::from_bytes(bytes)
    }

    #[test]
    fn names_are_nfc_composed_and_bad_bytes_refused_with_lossy_display() {
        // Decomposed e + combining acute composes to the precomposed form.
        #[cfg(unix)]
        let nfd = os(b"cafe\xcc\x81.txt");
        #[cfg(not(unix))]
        let nfd = OsStr::new("cafe\u{301}.txt");
        let got = admit_name(nfd).unwrap();
        assert_eq!(got, "caf\u{e9}.txt");

        #[cfg(unix)]
        {
            let bad = os(b"na\xffme");
            let err = admit_name(bad).unwrap_err();
            assert_eq!(err.reason, RefusalReason::NonUtf8Name);
            assert_eq!(err.display_name, "na\u{fffd}me");
        }
    }

    #[test]
    fn reserved_names_are_refused_with_component_as_display_name() {
        for n in ["aux.txt", "CON", "com1.tar.gz", "lpt9 "] {
            let err = admit_kind(n.to_string(), ObservedKind::File, None, 0).unwrap_err();
            assert_eq!(err.reason, RefusalReason::ReservedName, "{n}");
            assert_eq!(err.display_name, n);
        }
        // COM0 was never reserved; near-misses pass.
        assert!(admit_kind("com0.txt".to_string(), ObservedKind::File, None, 0).is_ok());
        assert!(admit_kind("auxiliary".to_string(), ObservedKind::File, None, 0).is_ok());
    }

    #[test]
    fn unrepresentable_types_are_refused_not_dropped() {
        let err = admit_kind("pipe".to_string(), ObservedKind::Other, None, 0).unwrap_err();
        assert_eq!(err.reason, RefusalReason::UnknownFileType);
    }

    #[test]
    fn files_and_dirs_admit_with_normalized_component() {
        let ok = admit_kind("caf\u{e9}.txt".to_string(), ObservedKind::File, None, 2).unwrap();
        assert_eq!(
            ok,
            AdmittedEntry {
                component: "caf\u{e9}.txt".into(),
                kind: AdmittedKind::File
            }
        );
        let ok = admit_kind("sub".to_string(), ObservedKind::Dir, None, 0).unwrap();
        assert_eq!(ok.kind, AdmittedKind::Dir);
    }

    #[test]
    fn symlink_policy_routes_through_the_gate_unchanged() {
        let t = |target: &OsStr, depth: usize| {
            admit_kind(
                "link".to_string(),
                ObservedKind::Symlink,
                Some(target),
                depth,
            )
        };
        // Internal relative target at any depth: sync as link, stored verbatim.
        let ok = t(OsStr::new("../real.txt"), 2).unwrap();
        assert_eq!(
            ok.kind,
            AdmittedKind::Symlink {
                target: "../real.txt".into()
            }
        );
        // Absolute target: refused, whatever the depth.
        assert_eq!(
            t(OsStr::new("/etc/passwd"), 0).unwrap_err().reason,
            RefusalReason::AbsoluteSymlinkTarget
        );
        // Escaping target: refuses exactly when it climbs above the root.
        assert_eq!(
            t(OsStr::new("../../out"), 1).unwrap_err().reason,
            RefusalReason::EscapingSymlinkTarget
        );
        assert!(t(OsStr::new("../../out"), 2).is_ok());
        assert_eq!(
            t(OsStr::new(".."), 0).unwrap_err().reason,
            RefusalReason::EscapingSymlinkTarget
        );
        #[cfg(unix)]
        {
            let err = t(os(b"\xff\xfe"), 0).unwrap_err();
            assert_eq!(err.reason, RefusalReason::NonUtf8SymlinkTarget);
        }
    }

    #[test]
    fn one_shot_gate_matches_the_two_phase_path() {
        fn facts(kind: ObservedKind, target: Option<&OsStr>) -> EntryFacts<'_> {
            EntryFacts {
                raw_name: OsStr::new("aux.txt"),
                kind,
                link_target: target,
                parent_depth: 0,
            }
        }
        assert_eq!(
            admit(facts(ObservedKind::File, None)),
            admit_kind(
                admit_name(OsStr::new("aux.txt")).unwrap(),
                ObservedKind::File,
                None,
                0
            )
        );
        assert_eq!(
            admit(facts(ObservedKind::Symlink, Some(OsStr::new("/abs")))),
            admit_kind(
                admit_name(OsStr::new("aux.txt")).unwrap(),
                ObservedKind::Symlink,
                Some(OsStr::new("/abs")),
                0
            )
        );
    }
}
