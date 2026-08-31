















use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use unicode_normalization::UnicodeNormalization;

use crate::config::IgnoreConfig;
use crate::defaults::DEFAULT_RULES;
use crate::error::IgnoreError;



pub fn is_quarantine_name(name: &str) -> bool {
    name.contains(".ferry-conflict.")
}




const FERRY_RULE_FILE: &str = "ferry.ignore";
const GIT_RULE_FILE: &str = ".gitignore";


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum V {
    Ign,
    Wl,
    No,
}



#[derive(Debug)]
pub struct FerryIgnore {
    root: PathBuf,
    cfg: IgnoreConfig,
    
    
    chain: Gitignore,
    
    
    skipped_lines: AtomicUsize,
    
    
    
    dirs: RwLock<HashMap<Vec<String>, Option<Gitignore>>>,
}

impl FerryIgnore {
    
    
    
    pub fn new(root: &Path, cfg: &IgnoreConfig) -> Result<Self, IgnoreError> {
        
        for id in &cfg.presets {
            if crate::presets::Preset::builtin(id).is_none() {
                return Err(IgnoreError::UnknownPreset(id.clone()));
            }
        }

        let mut builder = GitignoreBuilder::new(root);
        let mut skipped = 0usize;
        let add = |builder: &mut GitignoreBuilder, skipped: &mut usize, line: &str| {
            compile_line(builder, skipped, line)
        };

        for line in DEFAULT_RULES {
            add(&mut builder, &mut skipped, line);
        }
        if cfg.honor_gitignore {
            if let Some(text) = readable(&root.join(GIT_RULE_FILE)) {
                for line in text.lines() {
                    add(&mut builder, &mut skipped, line);
                }
            }
        }
        match read_rule_file(&root.join(FERRY_RULE_FILE)) {
            Some(Ok(text)) => {
                for line in text.lines() {
                    add(&mut builder, &mut skipped, line);
                }
            }
            Some(Err(source)) => {
                return Err(IgnoreError::ReadRootRule {
                    path: root.join(FERRY_RULE_FILE),
                    source,
                })
            }
            None => {}
        }
        for id in &cfg.presets {
            for line in crate::presets::Preset::builtin(id)
                .expect("validated above")
                .rule_lines()
            {
                add(&mut builder, &mut skipped, &line);
            }
        }
        for line in &cfg.overrides {
            add(&mut builder, &mut skipped, line);
        }

        let chain = builder
            .build()
            .map_err(|e| IgnoreError::Compile(e.to_string()))?;

        Ok(FerryIgnore {
            root: root.to_path_buf(),
            cfg: cfg.clone(),
            chain,
            skipped_lines: AtomicUsize::new(skipped),
            dirs: RwLock::new(HashMap::new()),
        })
    }

    
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines.load(Ordering::Relaxed)
    }

    
    
    
    
    
    
    
    
    
    
    
    
    
    pub fn decided(&self, rel: &[String], is_dir: bool) -> bool {
        if rel.is_empty() || rel.last().is_some_and(|n| is_quarantine_name(n)) {
            return false;
        }
        
        
        
        let normalized;
        let rel = if rel.iter().any(|c| !unicode_normalization::is_nfc(c)) {
            normalized = rel
                .iter()
                .map(|c| c.nfc().collect::<String>())
                .collect::<Vec<_>>();
            &normalized[..]
        } else {
            rel
        };
        let mut excluded_parent = false;
        let mut final_ignored = false;
        for depth in 1..=rel.len() {
            
            
            let dir_here = depth < rel.len() || is_dir;
            let mut v = Self::match_layer(&self.chain, &rel[..depth], dir_here);
            
            
            
            
            for j in 1..depth {
                if let Some(Some(gi)) = self.dir_overlay(&rel[..j]) {
                    let vv = Self::match_layer(&gi, &rel[j..depth], dir_here);
                    if vv != V::No {
                        v = vv;
                    }
                }
            }
            if depth < rel.len() {
                
                
                if v == V::Ign {
                    excluded_parent = true;
                }
            } else {
                final_ignored = excluded_parent || v == V::Ign;
            }
        }
        final_ignored
    }

    fn match_layer(gi: &Gitignore, prefix: &[String], is_dir: bool) -> V {
        let joined = prefix.join("/");
        match gi.matched(Path::new(&joined), is_dir) {
            ignore::Match::None => V::No,
            ignore::Match::Ignore(_) => V::Ign,
            ignore::Match::Whitelist(_) => V::Wl,
        }
    }

    
    
    
    fn dir_overlay(&self, dir: &[String]) -> Option<Option<Gitignore>> {
        if let Some(cached) = self.dirs.read().expect("dirs lock").get(dir) {
            return Some(cached.clone());
        }
        let loaded = self.load_dir_overlay(dir);
        self.dirs
            .write()
            .expect("dirs lock")
            .insert(dir.to_vec(), loaded.clone());
        Some(loaded)
    }

    fn load_dir_overlay(&self, dir: &[String]) -> Option<Gitignore> {
        let mut abs = self.root.clone();
        for c in dir {
            abs.push(c);
        }
        let mut builder = GitignoreBuilder::new(&abs);
        let mut skipped = 0usize;
        let mut any = false;
        if self.cfg.honor_gitignore {
            if let Some(text) = readable(&abs.join(GIT_RULE_FILE)) {
                for line in text.lines() {
                    if compile_line(&mut builder, &mut skipped, line) {
                        any = true;
                    }
                }
            }
        }
        if let Some(text) = readable(&abs.join(FERRY_RULE_FILE)) {
            for line in text.lines() {
                if compile_line(&mut builder, &mut skipped, line) {
                    any = true;
                }
            }
        }
        if skipped > 0 {
            self.skipped_lines.fetch_add(skipped, Ordering::Relaxed);
        }
        if !any {
            return None;
        }
        builder.build().ok()
    }
}




fn compile_line(builder: &mut GitignoreBuilder, skipped: &mut usize, line: &str) -> bool {
    let trimmed = line.trim_end();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    let nfc: String = trimmed.nfc().collect();
    match builder.add_line(None, &nfc) {
        Ok(_) => true,
        Err(_) => {
            *skipped += 1;
            false
        }
    }
}



fn readable(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}



fn read_rule_file(path: &Path) -> Option<Result<String, std::io::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(Ok(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e)),
    }
}

impl ferry_scan::IgnorePolicy for FerryIgnore {
    
    
    fn ignored(&self, rel: &[String], kind: ferry_scan::EntryKind) -> bool {
        self.decided(rel, kind == ferry_scan::EntryKind::Dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IgnoreConfig;

    
    
    fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        (tmp, root)
    }

    fn rel(path: &str) -> Vec<String> {
        path.split('/').map(str::to_string).collect()
    }

    fn ig(f: &FerryIgnore, path: &str) -> bool {
        f.decided(&rel(path), false)
    }
    fn ig_dir(f: &FerryIgnore, path: &str) -> bool {
        f.decided(&rel(path), true)
    }

    #[test]
    fn leading_slash_anchors_to_folder_root() {
        let (_t, root) = tree(&[("ferry.ignore", "/build\n/root.txt\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "build")); 
        assert!(
            !ig(&f, "deep/build"),
            "anchored pattern must not match deeper"
        );
        assert!(ig(&f, "root.txt"));
        assert!(!ig(&f, "deep/root.txt"));
    }

    #[test]
    fn unanchored_patterns_match_at_every_depth_but_not_across_slashes() {
        let (_t, root) = tree(&[("ferry.ignore", "*.rs\nfoo*bar\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "main.rs"));
        assert!(
            ig(&f, "sub/main.rs"),
            "slash-less patterns apply at every level"
        );
        
        
        assert!(ig(&f, "sub/mod/lib.rs"));
        
        assert!(ig(&f, "foobar"));
        assert!(ig(&f, "fooxbar"));
        assert!(!ig(&f, "foo/x/bar"));
    }

    #[test]
    fn directory_scoped_patterns_only_match_direct_children() {
        let (_t, root) = tree(&[("ferry.ignore", "sub/*.tmp\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "sub/a.tmp"));
        assert!(
            !ig(&f, "sub/deep/a.tmp"),
            "partial anchoring stops at one level"
        );
        assert!(!ig(&f, "a.tmp"));
    }

    #[test]
    fn trailing_slash_is_dir_only() {
        let (_t, root) = tree(&[("ferry.ignore", "build/\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig_dir(&f, "build"));
        assert!(ig_dir(&f, "deep/build"));
        assert!(!ig(&f, "build"), "a FILE named build must survive build/");
    }

    #[test]
    fn doublestar_forms() {
        let (_t, root) = tree(&[(
            "ferry.ignore",
            "**/logs\nlogs/**\na/**/b.md\n**/temp/*.cache\n",
        )]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        
        assert!(ig_dir(&f, "logs"));
        assert!(ig_dir(&f, "x/y/logs"));
        
        assert!(ig(&f, "logs/a.txt"));
        assert!(ig(&f, "x/y/logs/a.txt"));
        
        assert!(ig(&f, "a/b.md"));
        assert!(ig(&f, "a/m/n/b.md"));
        
        assert!(ig(&f, "temp/x.cache"));
        assert!(ig(&f, "q/temp/x.cache"));
        assert!(!ig(&f, "temp/d/x.cache"));
    }

    #[test]
    fn negation_last_match_wins_within_one_file() {
        let (_t, root) = tree(&[("ferry.ignore", "*.log\n!important.log\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "noise.log"));
        assert!(!ig(&f, "important.log"));
        
        let (_t2, root2) = tree(&[("ferry.ignore", "*.log\n!important.log\n!*.log\n")]);
        let f2 = FerryIgnore::new(&root2, &IgnoreConfig::default()).unwrap();
        assert!(!ig(&f2, "noise.log"));
    }

    #[test]
    fn nested_rule_file_overrides_root_chain_by_depth() {
        let (_t, root) = tree(&[
            ("ferry.ignore", "*.log\n"),
            ("sub/ferry.ignore", "!keep.log\n"),
        ]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "noise.log"));
        assert!(
            ig(&f, "sub/noise.log"),
            "root rule still applies where nested is silent"
        );
        assert!(
            !ig(&f, "sub/keep.log"),
            "deeper file wins inside its subtree"
        );
        assert!(ig(&f, "other/keep.log"), "...but only there");
    }

    #[test]
    fn nested_unanchored_patterns_reach_everything_beneath() {
        let (_t, root) = tree(&[("sub/deep/ferry.ignore", "*.cache\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "sub/deep/a.cache"));
        assert!(ig(&f, "sub/deep/more/b.cache"));
        assert!(!ig(&f, "sub/other.cache"));
    }

    #[test]
    fn cannot_reinclude_under_an_excluded_directory() {
        
        
        let (_t, root) = tree(&[("ferry.ignore", "build/\n!build/keep.txt\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "build/keep.txt"));
        
        let (_t2, root2) = tree(&[("ferry.ignore", "logs\n!logs/critical.log\n")]);
        let f2 = FerryIgnore::new(&root2, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f2, "logs/critical.log"));
    }

    #[test]
    fn gitignore_honored_only_when_opted_in() {
        let files = [("ferry.ignore", ""), (".gitignore", "*.secret\n")];
        let (_t, root) = tree(&files);
        let off = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(!ig(&off, "api.secret"), "default OFF");
        let on = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                honor_gitignore: true,
                ..IgnoreConfig::default()
            },
        )
        .unwrap();
        assert!(ig(&on, "api.secret"));
        assert!(ig(&on, "deep/api.secret"));
    }

    #[test]
    fn ferry_ignore_beats_gitignore_at_equal_depth() {
        let (_t, root) = tree(&[
            (".gitignore", "*.envish\n"),
            ("ferry.ignore", "!needed.envish\n"),
        ]);
        let cfg = IgnoreConfig {
            honor_gitignore: true,
            ..IgnoreConfig::default()
        };
        let f = FerryIgnore::new(&root, &cfg).unwrap();
        assert!(ig(&f, "other.envish"));
        assert!(
            !ig(&f, "needed.envish"),
            "ferry.ignore lines compile after .gitignore"
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let (_t, root) = tree(&[("ferry.ignore", "# a comment\n\n  \njunk.bin\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "junk.bin"));
        assert!(!ig(&f, "# a comment"));
    }

    #[test]
    fn invalid_glob_lines_are_skipped_not_fatal() {
        
        
        let (_t, root) = tree(&[("ferry.ignore", "[z-a]\njunk.bin\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "junk.bin"));
        assert_eq!(f.skipped_lines(), 1, "the bad line was counted");
    }

    #[test]
    fn defaults_exclude_os_and_editor_junk() {
        let (_t, root) = tree(&[("ferry.ignore", "")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, ".DS_Store"));
        assert!(ig(&f, "deep/.DS_Store"));
        assert!(ig(&f, "Thumbs.db"));
        assert!(ig(&f, "desktop.ini"));
        assert!(ig(&f, "main.rs.swp"));
        assert!(ig(&f, "notes.txt~"));
    }

    #[test]
    fn node_modules_out_by_default_and_optable_back_in() {
        let (_t, root) = tree(&[("ferry.ignore", "")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig_dir(&f, "node_modules"));
        assert!(ig(&f, "node_modules/pkg/index.js"));

        let (_t2, root2) = tree(&[("ferry.ignore", "!node_modules/\n")]);
        let f2 = FerryIgnore::new(&root2, &IgnoreConfig::default()).unwrap();
        assert!(
            !ig_dir(&f2, "node_modules"),
            "user opt-in beats default layer"
        );
        assert!(!ig(&f2, "node_modules/pkg/index.js"));
    }

    #[test]
    fn env_files_stay_out_until_explicitly_opted_in() {
        let (_t, root) = tree(&[("ferry.ignore", "")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, ".env"));
        assert!(ig(&f, ".env.local"));
        assert!(ig(&f, "deploy/.env.production"));

        
        let (_t2, root2) = tree(&[("ferry.ignore", "!.env\n")]);
        let f2 = FerryIgnore::new(&root2, &IgnoreConfig::default()).unwrap();
        assert!(!ig(&f2, ".env"));
        assert!(
            ig(&f2, ".env.local"),
            "opting in .env must not drag the family in"
        );
        assert!(
            !ig(&f2, "deploy/.env"),
            "negation without slash anchors to root"
        );
    }

    #[test]
    fn user_overrides_beat_defaults_and_ferry_ignore() {
        let (_t, root) = tree(&[("ferry.ignore", "*.key\n")]);
        let cfg = IgnoreConfig {
            overrides: vec!["!.DS_Store".into()],
            ..IgnoreConfig::default()
        };
        let f = FerryIgnore::new(&root, &cfg).unwrap();
        assert!(!ig(&f, ".DS_Store"), "overrides are the top layer");
        assert!(ig(&f, "Thumbs.db"), "siblings unaffected");
        assert!(
            ig(&f, "id.key"),
            "overrides absent => lower layers still apply"
        );
    }

    #[test]
    fn presets_beat_ferry_ignore_but_lose_to_overrides() {
        
        
        let (_t, root) = tree(&[
            ("ferry.ignore", "!telemetry/\n"),
            ("telemetry/ping.json", "{}\n"),
        ]);
        let cfg = IgnoreConfig {
            presets: vec!["claude".into()],
            overrides: vec!["!statsig/".into()],
            ..IgnoreConfig::default()
        };
        let f = FerryIgnore::new(&root, &cfg).unwrap();
        assert!(
            ig(&f, "telemetry/ping.json"),
            "preset (layer above ferry.ignore) wins"
        );
        assert!(!ig_dir(&f, "statsig"), "override beats preset exclusion");
        
        assert!(
            !ig(&f, "projects/proj/memory/notes.md"),
            "project memory travels despite sessions exclusion nearby"
        );
        assert!(ig(&f, "projects/proj/sessions/s1.jsonl"));
    }

    #[test]
    fn quarantine_conflict_files_cannot_be_ignored() {
        let (_t, root) = tree(&[("ferry.ignore", "*.ferry-conflict.*\nreport.log\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(
            !ig(&f, "doc.txt.ferry-conflict.devA-1727000000"),
            "quarantine files sync even against explicit rules"
        );
        assert!(ig(&f, "report.log"));
        assert!(is_quarantine_name("doc.txt.ferry-conflict.devA-1727000000"));
        assert!(!is_quarantine_name("conflicted.txt"));
    }

    #[test]
    fn pattern_lines_are_nfc_normalized_before_matching() {
        
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(
            root.join("ferry.ignore"),
            "cafe\u{301}.txt\n", 
        )
        .unwrap();

        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "caf\u{e9}.txt"), "NFD pattern must match NFC path");
        assert!(ig(&f, "cafe\u{301}.txt"));
        assert!(!ig(&f, "cafe.txt"));
    }

    #[test]
    fn decomposed_paths_are_treated_as_one_name_against_nfc_patterns() {
        
        
        
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("ferry.ignore"), "rapport-ann\u{e9}e.md\n").unwrap();
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(
            ig(&f, "rapport-anne\u{301}e.md"),
            "decomposed path must match composed pattern"
        );
        assert!(ig(&f, "rapport-ann\u{e9}e.md"));
        assert!(!ig(&f, "rapport-annee.md"));
    }

    #[test]
    fn unknown_preset_id_fails_loudly() {
        let (_t, root) = tree(&[]);
        let cfg = IgnoreConfig {
            presets: vec!["nope".into()],
            ..IgnoreConfig::default()
        };
        let err = FerryIgnore::new(&root, &cfg).unwrap_err();
        assert!(matches!(err, IgnoreError::UnknownPreset(_)));
    }

    #[test]
    fn missing_root_ferry_ignore_is_fine_unreadable_is_fatal() {
        let (_t, root) = tree(&[]);
        assert!(FerryIgnore::new(&root, &IgnoreConfig::default()).is_ok());

        
        std::fs::create_dir(root.join("ferry.ignore")).unwrap();
        let err = FerryIgnore::new(&root, &cfg_placeholder()).unwrap_err();
        assert!(matches!(err, IgnoreError::ReadRootRule { .. }));
    }

    fn cfg_placeholder() -> IgnoreConfig {
        IgnoreConfig::default()
    }

    #[test]
    fn seam_decisions_parameterized_over_entry_kind_need_no_disk() {
        
        
        let (_t, root) = tree(&[
            ("ferry.ignore", "build/\ncache\n*.log\n!important.log\n"),
            
            ("sub/ferry.ignore", "!build/\n"),
        ]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();

        let cases: &[(&str, bool, bool)] = &[
            
            
            ("build", false, true),
            ("deep/build", false, true),
            
            ("cache", true, true),
            ("deep/cache/x.bin", true, true),
            
            
            ("noise.log", true, true),
            ("important.log", false, false),
            ("deep/important.log", false, false),
            
            
            ("build/inner.txt", true, true),
            ("build/sub/deep.txt", true, true),
            
            ("sub/build", false, false),
            
            ("other/build", false, true),
            
            ("keep.txt", false, false),
        ];
        for (path, want_file, want_dir) in cases {
            assert_eq!(f.decided(&rel(path), false), *want_file, "{path} as file");
            assert_eq!(f.decided(&rel(path), true), *want_dir, "{path} as dir");
        }
    }

    #[test]
    fn trait_impl_delegates_to_decided_with_the_callers_kind() {
        let (_t, root) = tree(&[("ferry.ignore", "build/\n")]);
        std::fs::create_dir_all(root.join("build")).unwrap();
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        use ferry_scan::{EntryKind, IgnorePolicy as _};
        for (path, kind, want) in [
            ("build", EntryKind::File, false),
            ("build", EntryKind::Dir, true),
            
            
            ("build/inner.txt", EntryKind::File, true),
        ] {
            assert_eq!(f.ignored(&rel(path), kind), want, "{path:?} {kind:?}");
        }
    }

    #[test]
    fn trait_ignored_keeps_quarantine_names_at_every_kind() {
        let (_t, root) = tree(&[("ferry.ignore", "*.ferry-conflict.*\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        use ferry_scan::{EntryKind, IgnorePolicy as _};
        let q = rel("doc.txt.ferry-conflict.devA-1727000000");
        assert!(!f.ignored(&q, EntryKind::File));
        assert!(!f.ignored(&q, EntryKind::Dir));
    }

    #[test]
    fn perf_ten_thousand_patterns_answer_in_milliseconds() {
        use std::fmt::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        
        let mut text = String::with_capacity(1 << 17);
        for i in 0..2500 {
            let _ = writeln!(text, "/pkg/mod{i}/");
            let _ = writeln!(text, "**/gen{i}.cache");
            let _ = writeln!(text, "*.tmp{i}");
            let _ = writeln!(text, "assets/sprite{i}.bin");
        }
        std::fs::write(root.join("ferry.ignore"), text).unwrap();
        let built = std::time::Instant::now();
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        let build_ms = built.elapsed().as_millis();
        assert!(build_ms < 5_000, "compiling 10k patterns took {build_ms}ms");

        let started = std::time::Instant::now();
        let mut hits = 0usize;
        for i in 0..500usize {
            for (path, want) in [
                (format!("pkg/mod{i}/x.rs").as_str(), true),
                (format!("deep/nested/gen{i}.cache").as_str(), true),
                ("notes.tmp7".to_string().as_str(), true),
                (format!("assets/sprite{i}.bin").as_str(), true),
                (format!("src/main{i}.rs").as_str(), false),
                ("keep/me.txt".to_string().as_str(), false),
            ] {
                let got = ig(&f, path);
                assert_eq!(got, want, "{path}");
                if got {
                    hits += 1;
                }
            }
        }
        let elapsed = started.elapsed();
        
        assert!(
            elapsed.as_millis() < 10_000,
            "3000 queries took {}ms",
            elapsed.as_millis()
        );
        assert_eq!(hits, 2000);
    }
}
