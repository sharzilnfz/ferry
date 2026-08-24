//! Share-time secret-scan heuristic (SPEC M4; research archetype 9).
//!
//! Runs when a folder is about to be shared: any HIGH-RISK path that the
//! effective rules would INCLUDE (i.e. sync to peers) is flagged, and its
//! content is scanned for likely credentials. Warnings carry file, line
//! number, rule class, and a REDACTED preview — never the secret itself.
//!
//! Scope is deliberately narrow: content scanning applies only to high-risk
//! files (`.env*`, `*.pem`, `*.key`, `id_rsa*`, `credentials.json`,
//! `.npmrc`). A stray key-shaped string in ordinary source produces no
//! warning; test fixtures and generated code are too noisy otherwise.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use ferry_scan::IgnorePolicy;

use crate::policy::FerryIgnore;

/// What kind of risk a warning describes: either the PATH itself is
/// high-risk (would sync), or the CONTENT matched a credential pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningClass {
    /// `.env`-family file included for sync.
    EnvFile,
    /// Private-key material by name (`*.pem`, `*.key`, `id_rsa*`).
    PrivateKeyFile,
    /// Cloud/service credentials JSON (`credentials.json`).
    CredentialsJson,
    /// npm auth config (may contain `_authToken`).
    Npmrc,
    /// AWS access key id (`AKIA…`).
    AwsKey,
    /// OpenAI-style API key (`sk-…`).
    OpenAiKey,
    /// GitHub personal access token (`ghp_…`).
    GitHubToken,
    /// Slack token (`xox[baprs]-…`).
    SlackToken,
    /// PEM private-key header.
    PrivateKeyHeader,
    /// Generic `api_key/secret/token/password = value` assignment.
    GenericAssignment,
}

impl WarningClass {
    pub fn label(&self) -> &'static str {
        match self {
            WarningClass::EnvFile => "env-file-included",
            WarningClass::PrivateKeyFile => "private-key-file-included",
            WarningClass::CredentialsJson => "credentials-json-included",
            WarningClass::Npmrc => "npmrc-included",
            WarningClass::AwsKey => "aws-access-key",
            WarningClass::OpenAiKey => "openai-key",
            WarningClass::GitHubToken => "github-token",
            WarningClass::SlackToken => "slack-token",
            WarningClass::PrivateKeyHeader => "private-key-header",
            WarningClass::GenericAssignment => "generic-credential-assignment",
        }
    }
}

/// One share-time warning. `line` is 1-based (`None` for path-level
/// warnings). `preview` is redacted: first 4 characters plus length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub path: Vec<String>,
    pub line: Option<usize>,
    pub class: WarningClass,
    pub preview: String,
}

impl Warning {
    /// Loud, human-readable rendering (the CLI will print this at share time).
    pub fn message(&self) -> String {
        let loc = match self.line {
            Some(n) => format!(":{n} "),
            None => " ".to_string(),
        };
        format!(
            "SECRET RISK [{}] {}{}— {}",
            self.class.label(),
            self.path.join("/"),
            loc,
            self.preview
        )
    }
}

/// Classify a basename into a path-level high-risk class, if it is one.
pub fn classify_path(basename: &str) -> Option<WarningClass> {
    let name = basename.rsplit('/').next().unwrap_or(basename);
    if name == ".env" || (name.starts_with(".env.") && name.len() > ".env.".len()) {
        return Some(WarningClass::EnvFile);
    }
    // Extension match stays case-sensitive on purpose: real key files are
    // lowercase, and flagging "KEY" variants would change existing behavior.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    if name.ends_with(".pem") || name.ends_with(".key") || name.starts_with("id_rsa") {
        return Some(WarningClass::PrivateKeyFile);
    }
    if name == "credentials.json" {
        return Some(WarningClass::CredentialsJson);
    }
    if name == ".npmrc" {
        return Some(WarningClass::Npmrc);
    }
    None
}

/// Content patterns; ticket-specified classes, applied ONLY inside high-risk
/// files (see module docs). Slack tokens require ≥10 chars after the prefix —
/// the one deliberate tightening over the bare `xox[baprs]-` heuristic — to
/// keep prose like "xoxo-" from warning.
fn content_patterns() -> &'static [(WarningClass, Regex)] {
    static PATTERNS: LazyLock<Vec<(WarningClass, Regex)>> = LazyLock::new(|| {
        vec![
            (
                WarningClass::AwsKey,
                Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
            ),
            (
                WarningClass::OpenAiKey,
                Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(),
            ),
            (
                WarningClass::GitHubToken,
                Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap(),
            ),
            (
                WarningClass::SlackToken,
                Regex::new(r"xox[baprs]-[0-9A-Za-z-]{10,}").unwrap(),
            ),
            (
                WarningClass::PrivateKeyHeader,
                Regex::new(
                    r"-----BEGIN ((RSA|EC|DSA|OPENSSH|PGP|ENCRYPTED) )?PRIVATE KEY( BLOCK)?-----",
                )
                .unwrap(),
            ),
            (
                WarningClass::GenericAssignment,
                Regex::new(r"(?i)(api[_-]?key|secret|token|password)\s*[=:]\s*\S+").unwrap(),
            ),
        ]
    });
    &PATTERNS
}

/// Cap per-file content warnings so one pathological file cannot flood a
/// report; the path-level warning still names the file.
const MAX_CONTENT_WARNINGS_PER_FILE: usize = 32;

/// Files larger than this are scanned only in their first
/// [`SCAN_BYTE_CAP`] bytes (env/key files are small; huge ones are usually
/// data with an unlucky extension).
const SCAN_BYTE_CAP: u64 = 8 * 1024 * 1024;

/// Scan `root` for likely credentials in paths the effective `rules` would
/// SYNC (included paths only — excluded files never leave the machine, so
/// they are silent here by design). Deterministic order: sorted walk.
///
/// This is the loud half of the `.env` story: opting `.env` back into sync
/// is allowed, but share time says exactly what is about to travel.
pub fn scan_for_secrets(rules: &FerryIgnore, root: &Path) -> Vec<Warning> {
    let mut out = Vec::new();
    walk(rules, root, &mut Vec::new(), &mut out);
    out
}

fn walk(rules: &FerryIgnore, abs: &Path, rel: &mut Vec<String>, out: &mut Vec<Warning>) {
    // Unreadable subtree: nothing to warn about here.
    let Ok(entries) = std::fs::read_dir(abs) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    for name in names {
        let component: String = name.nfc().collect();
        let mut child_abs = abs.to_path_buf();
        child_abs.push(&name);
        // Vanished mid-walk.
        let Ok(meta) = std::fs::symlink_metadata(&child_abs) else {
            continue;
        };
        rel.push(component.clone());
        if meta.is_dir() {
            // Prune exactly like the scanner: ignored dirs are never synced,
            // so their contents can never leak at share time either.
            if !rules.ignored(rel) {
                walk(rules, &child_abs, rel, out);
            }
        } else if meta.is_file() {
            scan_file_if_risky(rules, rel, &child_abs, &meta, out);
        }
        // Symlinks: not followed, not scanned (walker stores them as links).
        rel.pop();
    }
}

fn scan_file_if_risky(
    rules: &FerryIgnore,
    rel: &[String],
    abs: &Path,
    meta: &std::fs::Metadata,
    out: &mut Vec<Warning>,
) {
    if rules.ignored(rel) {
        return; // would NOT sync → nothing leaves → no warning
    }
    let Some(path_class) = rel.last().and_then(|n| classify_path(n)) else {
        return;
    };

    // Path-level warning: this high-risk file WILL travel.
    out.push(Warning {
        path: rel.to_vec(),
        line: None,
        class: path_class,
        preview: format!(
            "{} included in sync ({} bytes)",
            rel.last().map_or("", String::as_str),
            meta.len()
        ),
    });

    // Content-level warnings: likely credentials inside.
    let Ok(mut bytes) = std::fs::read(abs) else {
        return;
    };
    bytes.truncate(SCAN_BYTE_CAP as usize);
    let text = String::from_utf8_lossy(&bytes);
    let mut count = 0usize;
    for (lineno, line) in text.lines().enumerate() {
        for (class, rx) in content_patterns() {
            if let Some(m) = rx.find(line) {
                out.push(Warning {
                    path: rel.to_vec(),
                    line: Some(lineno + 1),
                    class: *class,
                    preview: redact(m.as_str()),
                });
                count += 1;
                if count >= MAX_CONTENT_WARNINGS_PER_FILE {
                    return;
                }
            }
        }
    }
}

/// REDACTED preview: first 4 chars + length. Never the full secret.
fn redact(matched: &str) -> String {
    let total = matched.chars().count();
    let head: String = matched.chars().take(4).collect();
    format!("{head}…({total} chars)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IgnoreConfig;

    const AWS: &str = "AKIAIOSFODNN7EXAMPLE"; // canonical 20-char example key
    const OPENAI: &str = "sk-proj0123456789abcdefghij0123";
    const GH: &str = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef1234";
    const SLACK: &str = "xoxb-123456789012-ABCDEFabcdef";

    fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        (tmp, root)
    }

    fn classes(warnings: &[Warning]) -> Vec<WarningClass> {
        warnings.iter().map(|w| w.class).collect()
    }

    #[test]
    fn included_env_with_credentials_warns_at_share_time() {
        let (_t, root) = tree(&[(
            ".env",
            &format!("AWS_ACCESS_KEY_ID={AWS}\nOPENAI_API_KEY={OPENAI}\n"),
        )]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        assert!(classes(&ws).contains(&WarningClass::EnvFile), "{ws:?}");
        assert!(classes(&ws).contains(&WarningClass::AwsKey), "{ws:?}");
        assert!(classes(&ws).contains(&WarningClass::OpenAiKey), "{ws:?}");
        // Line numbers are 1-based and correct.
        let aws = ws.iter().find(|w| w.class == WarningClass::AwsKey).unwrap();
        assert_eq!(aws.line, Some(1));
        assert_eq!(aws.path, vec![".env".to_string()]);
    }

    #[test]
    fn excluded_env_is_silent_because_it_never_leaves() {
        let (_t, root) = tree(&[(".env", &format!("AWS_ACCESS_KEY_ID={AWS}\n"))]);
        let rules = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        let ws = scan_for_secrets(&rules, &root);
        assert!(
            ws.is_empty(),
            "excluded .env must produce NO warning: {ws:?}"
        );
    }

    #[test]
    fn clean_included_env_still_warns_about_the_path_itself() {
        let (_t, root) = tree(&[(".env", "APP_ENV=dev\nLOG_LEVEL=info\n")]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].class, WarningClass::EnvFile);
        assert_eq!(ws[0].line, None);
    }

    #[test]
    fn previews_are_redacted_and_carry_length() {
        let (_t, root) = tree(&[(".env", &format!("AWS_ACCESS_KEY_ID={AWS}\n"))]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        let aws = ws.iter().find(|w| w.class == WarningClass::AwsKey).unwrap();
        assert!(aws.preview.starts_with("AKIA"), "{}", aws.preview);
        assert!(!aws.preview.contains(AWS));
        assert!(aws.preview.contains(&format!("{} chars", AWS.len())));
    }

    #[test]
    fn all_ticket_pattern_classes_fire() {
        let body =
            format!("AWS={AWS}\nKEY={OPENAI}\nTOK={GH}\nSLACK={SLACK}\npassword=hunter2secret\n");
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n";
        let (_t, root) = tree(&[(".env.keys", &body), ("server.pem", pem)]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env.*".into(), "!server.pem".into(), "!*.pem".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        let cs = classes(&ws);
        for want in [
            WarningClass::AwsKey,
            WarningClass::OpenAiKey,
            WarningClass::GitHubToken,
            WarningClass::SlackToken,
            WarningClass::GenericAssignment,
            WarningClass::PrivateKeyFile,
            WarningClass::PrivateKeyHeader,
        ] {
            assert!(cs.contains(&want), "missing {want:?}: {ws:?}");
        }
    }

    #[test]
    fn generic_assignment_is_case_insensitive_and_flexible() {
        let (_t, root) = tree(&[(".env", "API_KEY=abcd1234\nmy_secret = top\nToken:\tzzz\n")]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        let generic = ws
            .iter()
            .filter(|w| w.class == WarningClass::GenericAssignment)
            .count();
        assert_eq!(generic, 3, "{ws:?}");
    }

    #[test]
    fn ordinary_source_files_are_out_of_scope() {
        let (_t, root) = tree(&[
            ("src/main.rs", &format!("// TODO remove {OPENAI}\n")),
            ("notes.md", "password=whatever\n"),
        ]);
        let rules = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(scan_for_secrets(&rules, &root).is_empty());
    }

    #[test]
    fn ignored_dirs_are_pruned_from_the_scan() {
        let (_t, root) = tree(&[("junk/.env", &format!("AWS={AWS}\n"))]);
        let rules = FerryIgnore::new(&root, &IgnoreConfig::default()).unwrap();
        assert!(scan_for_secrets(&rules, &root).is_empty());
    }

    #[test]
    fn quarantine_named_env_still_gets_scanned() {
        let (_t, root) = tree(&[(".env.ferry-conflict.devB-123", &format!("AWS={AWS}\n"))]);
        let rules = FerryIgnore::new(
            &root,
            &IgnoreConfig {
                overrides: vec!["!.env.*".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let ws = scan_for_secrets(&rules, &root);
        assert!(!ws.is_empty());
    }
}
