//! The ignore engine: faithful gitignore semantics layered into Ferry's
//! precedence model, exposed as [`FerryIgnore`] (implements
//! `ferry_scan::IgnorePolicy`).
//!
//! Matching is delegated to `ignore::gitignore::Gitignore::matched()` (see
//! crate docs for why). This module owns LAYERING:
//!
//! - The root-level chain (defaults → root `.gitignore` when honored → root
//!   `ferry.ignore` → applied presets → user overrides) compiles as ONE
//!   ordered gitignore; last-match-wins reproduces exactly the documented
//!   layer precedence.
//! - Per-directory rule files BELOW the root load lazily and answer queries
//!   with paths relative to their OWN directory, evaluated shallow-to-deep so
//!   deeper files override shallower ones (git's depth-first precedence), and
//!   an unanchored pattern in a nested file reaches everything beneath it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use unicode_normalization::UnicodeNormalization;

use crate::config::IgnoreConfig;
use crate::defaults::DEFAULT_RULES;
use crate::error::IgnoreError;

/// True for conflict-quarantine names (`path.ext.ferry-conflict.<dev>-<ts>`).
/// Quarantine files must sync (ADR-0004), so they are NEVER ignorable.
pub fn is_quarantine_name(name: &str) -> bool {
    name.contains(".ferry-conflict.")
}

/// Rule-file names consulted per directory. Within one directory,
/// `.gitignore` compiles FIRST and `ferry.ignore` SECOND so Ferry-specific
/// intent wins ties at equal depth.
const FERRY_RULE_FILE: &str = "ferry.ignore";
const GIT_RULE_FILE: &str = ".gitignore";

/// One matched-layer verdict, flattened from `ignore::Match`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum V {
    Ign,
    Wl,
    No,
}

/// Compiled rule set for one synced folder: the root-level layer chain plus
/// lazily loaded per-directory rule files below the root.
#[derive(Debug)]
pub struct FerryIgnore {
    root: PathBuf,
    cfg: IgnoreConfig,
    /// Root-level chain IN ORDER: defaults, root `.gitignore` (opt-in), root
    /// `ferry.ignore`, applied presets, user overrides.
    chain: Gitignore,
    /// Rule lines skipped because they were not valid gitignore globs. Git
    /// warns and continues; so do we, observably.
    skipped_lines: AtomicUsize,
    /// Per-directory rule-file matchers keyed by directory components;
    /// `None` caches "no rules here". Loaded on first touch of any path
    /// under that directory.
    dirs: RwLock<HashMap<Vec<String>, Option<Gitignore>>>,
}

impl FerryIgnore {
    /// Compile the folder's rule set. Reads the root `ferry.ignore` eagerly
    /// (loud error if present-but-unreadable); nested files load lazily and
    /// an unreadable nested file is treated as absent.
    pub fn new(root: &Path, cfg: &IgnoreConfig) -> Result<Self, IgnoreError> {
        // Unknown preset ids are typos; fail before anything is built.
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

    /// Number of rule lines skipped as invalid. Diagnostic only.
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines.load(Ordering::Relaxed)
    }

    /// Decide with an explicit directory/file interpretation of the final
    /// component (no disk stat). Table tests and power users use this; the
    /// [`ferry_scan::IgnorePolicy`] impl resolves ambiguity itself.
    ///
    /// Implements git's composite model:
    /// - within a layer set, last non-neutral verdict wins (deeper files
    ///   after shallower ones);
    /// - once any ANCESTOR dir verdict is Ignore, descendants stay ignored —
    ///   git does not re-include under an excluded directory (our walk
    ///   enforces the same by pruning);
    /// - quarantine-named final components always return false.
    pub fn decided(&self, rel: &[String], is_dir: bool) -> bool {
        if rel.is_empty() || rel.last().is_some_and(|n| is_quarantine_name(n)) {
            return false;
        }
        // Defensive NFC: the walker guarantees NFC components, but direct
        // callers (secret scan on raw disk names, tests) might not. Fast
        // path skips the copy when everything is already NFC.
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
            // Intermediate components are necessarily directories (the walk
            // descended through them); symlinks are never descended into.
            let dir_here = depth < rel.len() || is_dir;
            let mut v = Self::match_layer(&self.chain, &rel[..depth], dir_here);
            // Ancestor rule files, shallowest first: each may override the
            // chain or shallower files for this path. A nested file's
            // unanchored patterns reach every path beneath it, exactly like
            // nested .gitignore in git.
            for j in 1..depth {
                if let Some(Some(gi)) = self.dir_overlay(&rel[..j]) {
                    let vv = Self::match_layer(&gi, &rel[j..depth], dir_here);
                    if vv != V::No {
                        v = vv;
                    }
                }
            }
            if depth < rel.len() {
                // Sticky: a whitelisted child cannot revive an excluded
                // parent's subtree (git quirk, mirrored by our pruning).
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

    /// Matcher for the rule files living in directory `dir` (never the root —
    /// root files live in [`Self::chain`]). Loads on first use; caches
    /// absence as `None`.
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

    fn disk_path(&self, rel: &[String]) -> PathBuf {
        let mut p = self.root.clone();
        for c in rel {
            p.push(c);
        }
        p
    }
}

/// Normalize one candidate line to NFC and feed it to the builder. Returns
/// whether a real pattern was added; comments, blanks, and invalid globs are
/// skipped (invalid ones counted).
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

/// Read a rule file: `None` when absent or unreadable (nested files are
/// best-effort; only the ROOT ferry.ignore fails loudly).
fn readable(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `Some` only when the file exists; `Err` surfaces read failures for the
/// root rule file path.
fn read_rule_file(path: &Path) -> Option<Result<String, std::io::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(Ok(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e)),
    }
}

impl ferry_scan::IgnorePolicy for FerryIgnore {
    /// Walker/event-filter seam. The final component's dir/file nature is not
    /// part of the trait signature, so dir-only patterns are resolved by
    /// double evaluation: only when the two interpretations disagree do we
    /// spend one `symlink_metadata`. Vanished paths resolve as files
    /// (either answer is fine — the walker skips absent entries anyway).
    fn ignored(&self, rel: &[String]) -> bool {
        if rel.is_empty() || rel.last().is_some_and(|n| is_quarantine_name(n)) {
            return false;
        }
        let as_file = self.decided(rel, false);
        let as_dir = self.decided(rel, true);
        if as_file == as_dir {
            return as_file;
        }
        match std::fs::symlink_metadata(self.disk_path(rel)) {
            Ok(meta) if meta.is_dir() => as_dir,
            _ => as_file,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IgnoreConfig;

    /// Build a folder tree containing exactly the given files (path, text)
    /// and return its root. Rule files are ordinary members of `files`.
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
        assert!(ig(&f, "build")); // anchored match at root
        assert!(!ig(&f, "deep/build"), "anchored pattern must not match deeper");
        assert!(ig(&f, "root.txt"));
        assert!(!ig(&f, "deep/root.txt"));
    }

    #[test]
    fn unanchored_patterns_match_at_every_depth_but_not_across_slashes() {
        let (_t, root) = tree(&[("ferry.ignore", "*.rs\nfoo*bar\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "main.rs"));
        assert!(ig(&f, "sub/main.rs"), "slash-less patterns apply at every level");
        // Git matches slash-less patterns against the BASENAME at any depth,
        // so even a nested lib.rs is caught by `*.rs`.
        assert!(ig(&f, "sub/mod/lib.rs"));
        // But wildcards inside a pattern never cross '/' themselves.
        assert!(ig(&f, "foobar"));
        assert!(ig(&f, "fooxbar"));
        assert!(!ig(&f, "foo/x/bar"));
    }

    #[test]
    fn directory_scoped_patterns_only_match_direct_children() {
        let (_t, root) = tree(&[("ferry.ignore", "sub/*.tmp\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "sub/a.tmp"));
        assert!(!ig(&f, "sub/deep/a.tmp"), "partial anchoring stops at one level");
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
        // **/logs : logs dirs at any depth
        assert!(ig_dir(&f, "logs"));
        assert!(ig_dir(&f, "x/y/logs"));
        // logs/** : everything under any logs dir
        assert!(ig(&f, "logs/a.txt"));
        assert!(ig(&f, "x/y/logs/a.txt"));
        // a/**/b.md : zero or more directories between
        assert!(ig(&f, "a/b.md"));
        assert!(ig(&f, "a/m/n/b.md"));
        // **/temp/*.cache : * after ** still single-level
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
        // Re-excluding later re-wins again.
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
        assert!(ig(&f, "sub/noise.log"), "root rule still applies where nested is silent");
        assert!(!ig(&f, "sub/keep.log"), "deeper file wins inside its subtree");
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
        // Git's documented quirk: a parent exclusion makes children
        // unreachable regardless of negations.
        let (_t, root) = tree(&[("ferry.ignore", "build/\n!build/keep.txt\n")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "build/keep.txt"));
        // Same quirk when the parent was excluded by an unanchored dir name.
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
        assert!(!ig(&f, "needed.envish"), "ferry.ignore lines compile after .gitignore");
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
        // `[z-a]` is an invalid character range; globset refuses it. One bad
        // line must not blind the folder.
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
        assert!(!ig_dir(&f2, "node_modules"), "user opt-in beats default layer");
        assert!(!ig(&f2, "node_modules/pkg/index.js"));
    }

    #[test]
    fn env_files_stay_out_until_explicitly_opted_in() {
        let (_t, root) = tree(&[("ferry.ignore", "")]);
        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, ".env"));
        assert!(ig(&f, ".env.local"));
        assert!(ig(&f, "deploy/.env.production"));

        // Opt in the exact file; siblings stay excluded.
        let (_t2, root2) = tree(&[("ferry.ignore", "!.env\n")]);
        let f2 = FerryIgnore::new(&root2, &IgnoreConfig::default()).unwrap();
        assert!(!ig(&f2, ".env"));
        assert!(ig(&f2, ".env.local"), "opting in .env must not drag the family in");
        assert!(!ig(&f2, "deploy/.env"), "negation without slash anchors to root");
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
        assert!(ig(&f, "id.key"), "overrides absent => lower layers still apply");
    }

    #[test]
    fn presets_beat_ferry_ignore_but_lose_to_overrides() {
        // claude preset excludes telemetry/; ferry.ignore tries to keep it;
        // user override wins over both.
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
        assert!(ig(&f, "telemetry/ping.json"), "preset (layer above ferry.ignore) wins");
        assert!(!ig_dir(&f, "statsig"), "override beats preset exclusion");
        // Preset includes rescue from its own excludes.
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
        // Write the pattern in NFD; walker-supplied paths are NFC.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(
            root.join("ferry.ignore"),
            "cafe\u{301}.txt\n", // NFD: 'e' + combining acute
        )
        .unwrap();

        let f = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(ig(&f, "caf\u{e9}.txt"), "NFD pattern must match NFC path");
        assert!(ig(&f, "cafe\u{301}.txt"));
        assert!(!ig(&f, "cafe.txt"));
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

        // A directory named ferry.ignore makes read_to_string fail (EISDIR).
        std::fs::create_dir(root.join("ferry.ignore")).unwrap();
        let err = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap_err();
        assert!(matches!(err, IgnoreError::ReadRootRule { .. }));
    }
}
