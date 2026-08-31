const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const _: () = assert!(RESERVED.len() == 22);

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
