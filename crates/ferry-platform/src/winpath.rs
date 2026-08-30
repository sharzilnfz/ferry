






















use std::path::Path;



pub const MAX_PATH: usize = 260;

const EXTENDED_PREFIX: &str = "\\\\?\\";
const EXTENDED_UNC_PREFIX: &str = "\\\\?\\UNC\\";


pub fn is_extended_length(p: &Path) -> bool {
    let Some(s) = p.to_str() else {
        return false;
    };
    s.starts_with(EXTENDED_PREFIX)
}





pub fn needs_extended_length(p: &Path) -> bool {
    match windows_shape(p) {
        Some(_) => p.to_str().is_some_and(|s| s.chars().count() >= MAX_PATH),
        None => false,
    }
}



fn windows_shape(p: &Path) -> Option<bool> {
    let s = p.to_str()?;
    if s.starts_with(EXTENDED_PREFIX) {
        return None; 
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\\' && bytes[1] == b'\\' {
        return Some(true); 
    }
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        
        
        let absolute = bytes.len() >= 3 && (bytes[2] == b'\\' || bytes[2] == b'/');
        return if absolute { Some(false) } else { None };
    }
    None
}






pub fn extend_path(p: &Path) -> std::path::PathBuf {
    
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    let Some(unc) = windows_shape(p) else {
        return p.to_path_buf();
    };
    if s.chars().count() < MAX_PATH {
        return p.to_path_buf();
    }
    if unc {
        
        let body = s.trim_start_matches('\\').replace('/', "\\");
        format!("{EXTENDED_UNC_PREFIX}{body}").into()
    } else {
        let body = s.replace('/', "\\");
        format!("{EXTENDED_PREFIX}{body}").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn win(components: &[&str]) -> PathBuf {
        
        let joined = components.join("\\");
        PathBuf::from(joined)
    }

    #[test]
    fn const_max_path_is_the_documented_260() {
        assert_eq!(MAX_PATH, 260);
    }

    #[test]
    fn short_paths_pass_through_untouched() {
        assert_eq!(
            extend_path(Path::new(r"C:\Users\a\b.txt")),
            Path::new(r"C:\Users\a\b.txt")
        );
        assert!(!needs_extended_length(&win(&[r"C:\short", "file"])));
    }

    #[test]
    fn boundary_at_260_chars_gets_prefix_259_does_not() {
        
        let stem = "d".repeat(256);
        let short = format!(r"C:\{stem}");
        assert_eq!(short.chars().count(), 259);
        assert_eq!(extend_path(Path::new(&short)), Path::new(&short));

        
        let long = format!(r"C:\{stem}x");
        assert_eq!(long.chars().count(), 260);
        let got = extend_path(Path::new(&long));
        assert_eq!(got, PathBuf::from(format!(r"\\?\C:\{stem}x")));
        assert!(needs_extended_length(Path::new(&long)));
        
        assert_eq!(extend_path(&got), got);
        assert!(!needs_extended_length(&got));
    }

    #[test]
    fn deep_nesting_past_the_cap_is_prefixed_and_normalized() {
        let mut parts: Vec<String> = vec![r"C:\work".to_string()];
        for i in 0..12 {
            parts.push(format!("level-{i:02}-directory-component"));
        }
        parts.push("leaf.bin".to_string());
        let parts_ref: Vec<&str> = parts.iter().map(String::as_str).collect();
        let p = win(&parts_ref);
        assert!(p.to_string_lossy().chars().count() > 260);

        let got = extend_path(&p);
        let s = got.to_string_lossy();
        assert!(s.starts_with("\\\\?\\C:\\work\\"), "{s}");
        assert!(!s.contains('/'), "separators normalized: {s}");
    }

    #[test]
    fn unc_paths_become_unc_extended_form() {
        let mut s = String::from(r"\\server\share");
        while s.chars().count() < 280 {
            s.push_str("\\nested");
        }
        let got = extend_path(Path::new(&s));
        let expect = format!(
            "\\\\?\\UNC\\{}",
            s.trim_start_matches('\\').replace('/', "\\")
        );
        assert_eq!(got, PathBuf::from(expect));
    }

    #[test]
    fn relative_posix_and_drive_relative_paths_are_never_touched() {
        let rel = "a/b/c";
        assert_eq!(extend_path(Path::new(rel)), Path::new(rel));

        let posix_long = format!("/{}", "d".repeat(400));
        assert_eq!(extend_path(Path::new(&posix_long)), Path::new(&posix_long));
        assert!(!needs_extended_length(Path::new(&posix_long)));

        
        let drive_rel = format!("C:{}", "d".repeat(400));
        assert_eq!(extend_path(Path::new(&drive_rel)), Path::new(&drive_rel));
    }

    #[test]
    fn already_prefixed_paths_are_recognized() {
        let pre = Path::new(r"\\?\C:\anything\even\short");
        assert!(is_extended_length(pre));
        assert_eq!(extend_path(pre), pre);
        assert!(!needs_extended_length(pre));

        let pre_unc = Path::new(r"\\?\UNC\server\share\x");
        assert!(is_extended_length(pre_unc));
        assert_eq!(extend_path(pre_unc), pre_unc);

        assert!(!is_extended_length(Path::new(r"C:\plain")));
    }

    #[test]
    fn non_utf8_paths_pass_through_without_panic() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let weird = std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe").to_owned();
            assert_eq!(extend_path(weird.as_ref()), weird);
        }
    }
}
