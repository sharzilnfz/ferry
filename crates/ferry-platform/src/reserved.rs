//! Windows reserved device names.
//!
//! DOS heritage: `CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`, `LPT1`..`LPT9`
//! are device names on Windows, matched case-insensitively and BEFORE any
//! extension — `CON.txt` collides with the console device on classic Win32
//! name resolution (Windows 11 relaxed some cases; older hosts remain
//! broken). A file named `aux.txt` created on Linux therefore cannot be
//! materialized on a Windows endpoint, ever.
//!
//! Policy (decided in T-012): **refuse loudly at both scan and materialize**
//! with an actionable message naming the entry and suggesting a rename. The
//! alternative — carrying the entry in manifests and failing only when a
//! Windows peer tries to write it — converts an immediate, local error into
//! a delayed cross-device one for zero benefit: such an entry can never be
//! represented on Windows regardless of what we do.
//!
//! Note COM0/LPT0 are deliberately absent: they were never reserved.

/// Exact reserved stems, upper-case.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", // COM10+ are NOT reserved by the classic rule.
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

// The array above must have exactly 22 entries; the split literal keeps the
// comment readable while the assert pins it.
const _: () = assert!(RESERVED.len() == 22);

/// Is this stored component a reserved Windows device name? Extension-
/// insensitive (`con.txt` → reserved), case-insensitive, trailing-space
/// tolerant (Win32 ignores trailing dots/spaces when resolving these).
pub fn is_reserved_device_name(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or(component);
    let base = base.trim_end_matches([' ', '.']);
    RESERVED.iter().any(|r| base.eq_ignore_ascii_case(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_classic_device_name_is_reserved() {
        for n in [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM5", "COM9", "LPT1", "LPT9",
        ] {
            assert!(is_reserved_device_name(n), "{n}");
        }
        // COM0 / LPT0 / COM10 were never reserved.
        assert!(!is_reserved_device_name("COM0"));
        assert!(!is_reserved_device_name("LPT0"));
        assert!(!is_reserved_device_name("COM10"));
        assert!(!is_reserved_device_name("COM0.txt"));
    }

    #[test]
    fn extensions_and_case_do_not_hide_the_collision() {
        assert!(is_reserved_device_name("con.txt"));
        assert!(is_reserved_device_name("Aux.tar.gz"));
        assert!(is_reserved_device_name("nul"));
        assert!(is_reserved_device_name("NUL."));
        assert!(is_reserved_device_name("prn "));
        assert!(is_reserved_device_name("Com1.dat"));
    }

    #[test]
    fn ordinary_names_pass() {
        for n in [
            "console",
            "auxiliary",
            "nullify",
            "com",
            "lpt",
            "printer",
            "auxx",
            "a.CON",
            "notes.md",
            "",
            " ",
        ] {
            assert!(!is_reserved_device_name(n), "{n}");
        }
    }
}
