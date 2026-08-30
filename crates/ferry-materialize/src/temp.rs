


































use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::{io_at, MaterializeError};


pub const TEMP_SUFFIX: &str = ".tmp";


pub const ENTROPY_HEX_LEN: usize = 8;




const NAME_LEN_LIMIT: usize = 200;



pub const DEFAULT_STALE_TEMP_AGE_SECS: u64 = 24 * 60 * 60;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempStyle {
    
    Dot,
    
    Windows,
}

impl TempStyle {
    
    pub fn current() -> Self {
        if cfg!(windows) {
            TempStyle::Windows
        } else {
            TempStyle::Dot
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            TempStyle::Dot => ".ferry.",
            TempStyle::Windows => "~ferry~",
        }
    }
}



pub fn temp_file_name(dest_name: &str, style: TempStyle, entropy: &str) -> String {
    debug_assert!(
        !dest_name.contains('/'),
        "temp_file_name takes the final component"
    );
    let mut s = String::with_capacity(
        style.prefix().len() + dest_name.len() + TEMP_SUFFIX.len() + entropy.len() + 1,
    );
    s.push_str(style.prefix());
    s.push_str(dest_name);
    s.push_str(TEMP_SUFFIX);
    if !entropy.is_empty() {
        s.push('.');
        s.push_str(entropy);
    }
    s
}




pub fn hashed_temp_file_name(rel_path: &str, style: TempStyle) -> String {
    let digest = blake3::hash(rel_path.as_bytes());
    format!(
        "{}{}{}",
        style.prefix(),
        &digest.to_hex()[..16],
        TEMP_SUFFIX
    )
}




pub fn temp_name_for(rel_path: &str, style: TempStyle, entropy: &str) -> String {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let candidate = temp_file_name(name, style, entropy);
    if candidate.len() > NAME_LEN_LIMIT || name.is_empty() {
        hashed_temp_file_name(rel_path, style)
    } else {
        candidate
    }
}






pub fn is_temp_name(name: &str) -> bool {
    [TempStyle::Dot, TempStyle::Windows]
        .iter()
        .any(|&style| matches_style(name, style))
}

fn matches_style(name: &str, style: TempStyle) -> bool {
    let Some(rest) = name.strip_prefix(style.prefix()) else {
        return false;
    };
    
    if rest.len() > TEMP_SUFFIX.len() && rest.ends_with(TEMP_SUFFIX) {
        return true;
    }
    
    if let Some(pos) = rest.rfind(TEMP_SUFFIX) {
        if let Some(ent) = rest[pos + TEMP_SUFFIX.len()..].strip_prefix('.') {
            return ent.len() == ENTROPY_HEX_LEN && ent.bytes().all(|b| b.is_ascii_hexdigit());
        }
    }
    false
}

fn hex_entropy() -> String {
    use rand::Rng;
    let mut bytes = [0u8; ENTROPY_HEX_LEN / 2];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}


pub fn fresh_entropy() -> String {
    hex_entropy()
}








pub fn sweep_stale_temps(
    target_root: &Path,
    max_age: Duration,
) -> Result<Vec<PathBuf>, MaterializeError> {
    let cutoff =
        SystemTime::now()
            .checked_sub(max_age)
            .ok_or_else(|| MaterializeError::BadComponent {
                component: "max_age overflow".into(),
            })?;
    let mut removed = Vec::new();
    let mut stack = vec![target_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_at(&dir, e)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&path, e)),
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_temp_name(&name) {
                continue;
            }
            let modified = match std::fs::symlink_metadata(&path) {
                Ok(m) => m.modified(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&path, e)),
            };
            match modified {
                Ok(t) if t < cutoff => match std::fs::remove_file(&path) {
                    Ok(()) => removed.push(path),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_at(&path, e)),
                },
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_at(&path, e)),
            }
        }
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mangling_matches_documented_patterns_both_styles() {
        assert_eq!(
            temp_file_name("main.rs", TempStyle::Dot, ""),
            ".ferry.main.rs.tmp"
        );
        assert_eq!(
            temp_file_name("main.rs", TempStyle::Windows, ""),
            "~ferry~main.rs.tmp"
        );
        
        assert_eq!(
            temp_file_name("a.txt", TempStyle::Dot, "0123abcd"),
            ".ferry.a.txt.tmp.0123abcd"
        );
        
        assert_eq!(
            temp_file_name("café.txt", TempStyle::Dot, ""),
            ".ferry.café.txt.tmp"
        );
    }

    #[test]
    fn current_style_follows_host_cfg() {
        #[cfg(windows)]
        assert_eq!(TempStyle::current(), TempStyle::Windows);
        #[cfg(not(windows))]
        assert_eq!(TempStyle::current(), TempStyle::Dot);
    }

    #[test]
    fn long_names_fall_back_to_hash_substitution() {
        
        
        let long_rel = format!("{}/{}.rs", "d".repeat(80), "n".repeat(195));
        let plain = temp_file_name(long_rel.rsplit('/').next().unwrap(), TempStyle::Dot, "");
        assert!(plain.len() > NAME_LEN_LIMIT);

        let picked = temp_name_for(&long_rel, TempStyle::Dot, "00112233");
        assert!(picked.len() <= NAME_LEN_LIMIT, "{picked}");
        assert_eq!(picked, hashed_temp_file_name(&long_rel, TempStyle::Dot));
        
        
        assert_eq!(picked, temp_name_for(&long_rel, TempStyle::Dot, "x"));
        assert_ne!(
            hashed_temp_file_name(&long_rel, TempStyle::Dot),
            hashed_temp_file_name(&format!("{long_rel}2"), TempStyle::Dot)
        );
        assert_ne!(
            hashed_temp_file_name(&long_rel, TempStyle::Dot),
            hashed_temp_file_name(&long_rel, TempStyle::Windows)
        );
        assert!(is_temp_name(&picked));
    }

    #[test]
    fn is_temp_name_accepts_every_documented_form_and_nothing_else() {
        
        assert!(is_temp_name(".ferry.x.tmp"));
        assert!(is_temp_name(".ferry.x.tmp.deadbeef"));
        assert!(is_temp_name("~ferry~x.tmp"));
        assert!(is_temp_name("~ferry~x.tmp.DEADBEEF"));
        
        assert!(!is_temp_name("x.tmp"));
        assert!(!is_temp_name(".ferry"));
        assert!(!is_temp_name(".ferryx.tmp")); 
        assert!(!is_temp_name("~ferry~x.tmp.short")); 
        assert!(!is_temp_name("~ferry~x.tmp.nothex!"));
        assert!(!is_temp_name(".ferry..tmp")); 
                                               
                                               
        assert!(!is_temp_name(".ferry.notes.tmp.b"));
    }

    #[test]
    fn sweep_removes_only_aged_temps_and_leaves_live_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join(".ferry.old.tmp.1234abcd"), b"stale").unwrap();
        std::fs::write(root.join("~ferry~old2.tmp"), b"stale win-style").unwrap();
        std::fs::write(root.join(".ferry.fresh.tmp.aabbccdd"), b"fresh").unwrap();
        std::fs::write(root.join("real.txt"), b"user data").unwrap();
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("subdir/.ferry.deep.tmp"), b"stale deep").unwrap();

        
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        for name in [
            ".ferry.old.tmp.1234abcd",
            "~ferry~old2.tmp",
            "subdir/.ferry.deep.tmp",
        ] {
            let f = std::fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }

        let removed = sweep_stale_temps(root, Duration::from_mins(1)).unwrap();
        let names: Vec<String> = removed
            .iter()
            .map(|p| {
                
                
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ".ferry.old.tmp.1234abcd",
                "subdir/.ferry.deep.tmp",
                "~ferry~old2.tmp"
            ]
        );
        assert!(!root.join(".ferry.old.tmp.1234abcd").exists());
        assert!(root.join(".ferry.fresh.tmp.aabbccdd").exists());
        assert!(root.join("real.txt").exists());

        
        assert_eq!(DEFAULT_STALE_TEMP_AGE_SECS, 86_400);
    }

    #[test]
    fn sweep_tolerates_vanishing_entries_mid_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".ferry.gone.tmp"), b"x").unwrap();
        
        let removed = sweep_stale_temps(dir.path(), Duration::from_hours(1)).unwrap();
        
        assert!(removed.is_empty());
    }
}
