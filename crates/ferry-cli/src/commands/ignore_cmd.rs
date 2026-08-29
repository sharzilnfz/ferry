//! `ferry ignore`: append pattern lines, apply agent-state presets, and
//! show the effective rule layers with precedence annotations.
//!
//! Layer order (ferry-ignore crate docs, mirrored verbatim here):
//! defaults < root ferry.ignore < applied presets < user overrides.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder::{self, save_settings, Settings};
use crate::out::Output;

pub fn run(
    folder: &Path,
    pattern: Option<&str>,
    preset: Option<&str>,
    list: bool,
) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let mut settings = opened.settings.clone();

    if list {
        return show_layers(&opened.root, &settings);
    }

    if let Some(name) = preset {
        let p = ferry_ignore::Preset::builtin(name).ok_or_else(|| {
            CliError::new(
                "unknown-preset",
                format!("no preset named {name:?}"),
                "built-in presets: claude, opencode (`ferry ignore --list` shows what they do)",
            )
        })?;
        if !settings.presets.iter().any(|x| x == name) {
            settings.presets.push(name.to_string());
            save_settings(&opened.root, &settings)?;
        }
        let json_doc = json!({
            "command": "ignore",
            "action": "applied-preset",
            "preset": name,
            "folder": opened.root.display().to_string(),
            "description": p.description,
            "rules_file": null,
        });
        let human = format!(
            "Applied preset {name:?}: {}\n  {} rule line(s) now active (layer: presets).\nNext: `ferry ignore --list` to see the effective layers.",
            p.description,
            p.rule_lines().len()
        );
        return Ok(Output::new(json_doc, human));
    }

    if let Some(line) = pattern {
        validate_pattern(line)?;
        append_rule_line(&opened.root, line)?;
        let json_doc = json!({
            "command": "ignore",
            "action": "added-line",
            "pattern": line,
            "folder": opened.root.display().to_string(),
            "preset": serde_json::Value::Null,
            "rules_file": opened.root.join("ferry.ignore").display().to_string(),
        });
        let human = format!(
            "Added {:?} to {} (layer: your ferry.ignore).\nNote: it cannot override built-in defaults for paths under an excluded directory.\nNext: `ferry ignore --list` to verify.",
            line,
            opened.root.join("ferry.ignore").display()
        );
        return Ok(Output::new(json_doc, human));
    }

    Err(CliError::new(
        "usage",
        "nothing to do: pass a PATTERN, --preset NAME, or --list",
        "`ferry ignore --help` shows the forms",
    ))
}

/// Reject lines that can never compile as gitignore globs before they land
/// in the file (the scanner also skips bad lines, but silently).
fn validate_pattern(line: &str) -> CliResult<()> {
    if line.trim().is_empty() {
        return Err(CliError::new(
            "bad-pattern",
            "empty ignore line",
            "write a gitignore-style glob like `dist/` or `*.log`",
        ));
    }
    if line.starts_with('#') {
        return Err(CliError::new(
            "bad-pattern",
            "comments are not patterns",
            "edit ferry.ignore directly to manage comments",
        ));
    }
    // `[z-a]`-style inverted ranges are the classic invalid glob; probe
    // compile through the real engine on a throwaway set.
    let probe = ferry_ignore::FerryIgnore::new(
        Path::new("/"),
        &ferry_ignore::IgnoreConfig {
            overrides: vec![line.to_string()],
            ..Default::default()
        },
    );
    match probe {
        Ok(f) if f.skipped_lines() == 0 => Ok(()),
        Ok(_) => Err(CliError::new(
            "bad-pattern",
            format!("{line:?} is not a valid gitignore glob"),
            "check character ranges and escaping; see gitignore docs",
        )),
        Err(e) => Err(CliError::new(
            "bad-pattern",
            e.to_string(),
            "fix the glob syntax",
        )),
    }
}

fn append_rule_line(root: &Path, line: &str) -> CliResult<()> {
    use std::io::Write;
    let path = root.join("ferry.ignore");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            CliError::new(
                "io",
                format!("cannot open {}: {e}", path.display()),
                "check permissions",
            )
        })?;
    writeln!(f, "{line}").map_err(|e| {
        CliError::new(
            "io",
            format!("cannot write {}: {e}", path.display()),
            "check disk space",
        )
    })
}

fn show_layers(root: &Path, settings: &Settings) -> CliResult<Output> {
    // Recompile layer by layer so each can be annotated. The engine takes a
    // whole config; here we build per-layer line lists directly instead.
    let layers: Vec<(&str, Vec<String>)> = vec![
        (
            "defaults (built-in)",
            ferry_ignore::defaults::DEFAULT_RULES
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        ),
        ("file ferry.ignore", read_lines(&root.join("ferry.ignore"))),
        (
            "presets (applied)",
            settings
                .presets
                .iter()
                .filter_map(|id| ferry_ignore::Preset::builtin(id))
                .flat_map(|p| {
                    let mut v = vec![format!("# preset: {}", p.id)];
                    v.extend(p.rule_lines());
                    v
                })
                .collect(),
        ),
        (
            "overrides (.ferry/settings.json)",
            settings.overrides.clone(),
        ),
    ];

    let json_doc = json!({
        "command": "ignore",
        "action": "list",
        "folder": root.display().to_string(),
        "layers": layers.iter().filter(|(_, l)| !l.is_empty()).map(|(name, lines)| json!({
            "name": name,
            "lines": lines,
        })).collect::<Vec<_>>(),
        "honor_gitignore": settings.honor_gitignore,
        "applied_presets": settings.presets,
    });

    let mut human = String::from(
        "# Effective selective rules. Within/across layers the LAST matching line wins;\n# deeper nested rule files override everything above for their subtree.\n",
    );
    for (name, lines) in &layers {
        if lines.is_empty() {
            continue;
        }
        let _ = writeln!(human, "\n[{name}]");
        for l in lines {
            let _ = writeln!(human, "  {l}");
        }
    }

    Ok(Output::new(json_doc, human))
}

fn read_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty() && !l.trim_start().starts_with('#'))
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}
