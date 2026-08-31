use std::ffi::OsStr;

use ferry_platform::{classify_link, is_reserved_device_name};
use unicode_normalization::UnicodeNormalization;

pub use crate::snapshot::RefusalReason;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedKind {
    File,
    Dir,
    Symlink,

    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: RefusalReason,

    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedEntry {
    pub component: String,
    pub kind: AdmittedKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmittedKind {
    File,
    Dir,
    Symlink { target: String },
}

#[derive(Clone, Debug)]
pub struct EntryFacts<'a> {
    pub raw_name: &'a OsStr,
    pub kind: ObservedKind,

    pub link_target: Option<&'a OsStr>,

    pub parent_depth: usize,
}

pub fn admit_name(raw: &OsStr) -> Result<String, Refusal> {
    match std::str::from_utf8(raw.as_encoded_bytes()) {
        Ok(s) => Ok(s.nfc().collect::<String>()),

        Err(_) => Err(Refusal {
            reason: RefusalReason::NonUtf8Name,
            display_name: String::from_utf8_lossy(raw.as_encoded_bytes()).into_owned(),
        }),
    }
}

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

    if is_reserved_device_name(&component) {
        return refuse(RefusalReason::ReservedName);
    }

    let admitted = match kind {
        ObservedKind::File => AdmittedKind::File,
        ObservedKind::Dir => AdmittedKind::Dir,
        ObservedKind::Other => {
            return refuse(RefusalReason::UnknownFileType);
        }
        ObservedKind::Symlink => {
            let Some(raw) = link_target else {
                return refuse(RefusalReason::NonUtf8SymlinkTarget);
            };
            let Some(t) = raw.to_str() else {
                return refuse(RefusalReason::NonUtf8SymlinkTarget);
            };

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

        let ok = t(OsStr::new("../real.txt"), 2).unwrap();
        assert_eq!(
            ok.kind,
            AdmittedKind::Symlink {
                target: "../real.txt".into()
            }
        );

        assert_eq!(
            t(OsStr::new("/etc/passwd"), 0).unwrap_err().reason,
            RefusalReason::AbsoluteSymlinkTarget
        );

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
