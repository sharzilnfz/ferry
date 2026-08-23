//! Agent-state presets (research archetype 8): curated include/exclude rule
//! sets for folders that exist to serve coding agents.
//!
//! A preset is DATA, not code: it round-trips through JSON (`serde`) so a
//! future CLI can persist exactly what was applied, and users can diff what
//! changed. Its [`Preset::rule_lines`] compiles to plain gitignore lines —
//! excludes first, then includes as `!` negations — so last-match-wins gives
//! includes the power to rescue sync-worthy state from broad OUT globs
//! (e.g. a stray `.log` inside `memory/` stays, because `!memory/**` comes
//! later than `**/*.log`).
//!
//! # Sources (research/use-cases.md, archetype 8)
//!
//! Five independent hand-rolled solutions for `~/.claude` agree on the split
//! this preset encodes: sync-worthy state (memory, skills, commands, agents,
//! settings, CLAUDE.md) vs machine-specific churn (session logs, telemetry,
//! statsig, caches, downloads):
//!
//! - github.com/anthropics/claude-code#25739 — portable project memory is THE
//!   pain; memory paths derive from absolute project paths.
//! - blog.lhotka.net (Claude Memory Sync) — merge-aware sync of
//!   `~/.claude/projects/*/memory/`; that exact glob is an include here.
//! - nickang.com — whitelists CLAUDE.md/settings/memory/skills/plugin config;
//!   excludes session logs, telemetry, caches as "machine-specific junk".
//!
//! OpenCode mirrors the same shape (AGENTS.md, config, skill/command dirs
//! in; session logs and caches out).
//!
//! Presets sit ABOVE root `ferry.ignore` and BELOW user overrides in the
//! layer chain (crate docs); nested per-directory files still win by depth.

/// One named agent-state preset: what to carry (`includes`) and what to leave
/// behind (`excludes`), both verbatim gitignore syntax WITHOUT leading `!`
/// (compilation adds it for includes).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preset {
    /// Stable identifier (`"claude"`, `"opencode"`).
    pub id: String,
    /// Human-facing summary, carried through serialization.
    pub description: String,
    /// Paths/dirs that MUST travel (compiled as `!` negations).
    pub includes: Vec<String>,
    /// Churn/machine-specific paths kept out of sync (compiled verbatim).
    pub excludes: Vec<String>,
}

impl Preset {
    /// The documented `~/.claude` selective-rules set.
    ///
    /// IN: `CLAUDE.md`, `settings.json`, `memory/**`, `skills/**`,
    /// `commands/**`, `agents/**`, `projects/*/memory/**`.
    /// OUT: `projects/**/sessions/**`, `**/*.log`, `statsig/`, `telemetry/`,
    /// `cache/`, `shell-snapshots/`, `downloads/`.
    pub fn claude() -> Self {
        Preset {
            id: "claude".into(),
            description: "~/.claude agent state: memory/skills/commands/settings in; \
                          session logs, telemetry, statsig, caches out."
                .into(),
            includes: vec![
                "CLAUDE.md".into(),
                "settings.json".into(),
                "memory/**".into(),
                "skills/**".into(),
                "commands/**".into(),
                "agents/**".into(),
                // Lhotka's merge-aware sync target; project memory travels
                // even though the sessions beside it never do.
                "projects/*/memory/**".into(),
            ],
            excludes: vec![
                "projects/**/sessions/**".into(),
                "**/*.log".into(),
                "statsig/".into(),
                "telemetry/".into(),
                "cache/".into(),
                "shell-snapshots/".into(),
                "downloads/".into(),
            ],
        }
    }

    /// The documented `.opencode` selective-rules set (same shape as
    /// [`Preset::claude`]): AGENTS.md/config/skill-command-plugin dirs in;
    /// session logs, caches, vendored deps out.
    pub fn opencode() -> Self {
        Preset {
            id: "opencode".into(),
            description: ".opencode agent state: AGENTS.md/config/memory/skills in; \
                          session logs, caches out."
                .into(),
            includes: vec![
                "AGENTS.md".into(),
                "opencode.json".into(),
                "agent/**".into(),
                "agents/**".into(),
                "command/**".into(),
                "commands/**".into(),
                "plugin/**".into(),
                "plugins/**".into(),
                "skill/**".into(),
                "skills/**".into(),
                "memory/**".into(),
            ],
            excludes: vec![
                "sessions/**".into(),
                "**/*.log".into(),
                "cache/".into(),
                "tmp/".into(),
                // Plugin installs may vendor deps; reinstallable, churning.
                "node_modules/".into(),
            ],
        }
    }

    /// All built-in presets, by id (`"claude"`, `"opencode"`).
    pub fn builtin(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Preset::claude()),
            "opencode" => Some(Preset::opencode()),
            _ => None,
        }
    }

    /// Compile to ordered gitignore lines: excludes first, then includes as
    /// `!` negations (so includes outrank excludes under last-match-wins).
    pub fn rule_lines(&self) -> Vec<String> {
        self.excludes
            .iter()
            .cloned()
            .chain(self.includes.iter().map(|i| format!("!{i}")))
            .collect()
    }

    /// Serialize to pretty JSON (stable field order; round-trip safe).
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("preset serializes")
    }

    /// Deserialize from JSON.
    pub fn from_json(json: &str) -> Result<Self, crate::error::IgnoreError> {
        serde_json::from_str(json).map_err(|e| crate::error::IgnoreError::PresetJson(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_preset_round_trips_through_json_unchanged() {
        let p = Preset::claude();
        let json = p.to_json_pretty();
        let back = Preset::from_json(&json).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.to_json_pretty(), json, "serialization is stable");
    }

    #[test]
    fn opencode_preset_round_trips_through_json_unchanged() {
        let p = Preset::opencode();
        let back = Preset::from_json(&p.to_json_pretty()).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn rule_lines_put_includes_after_excludes() {
        let lines = Preset::claude().rule_lines();
        let incl_start = lines.iter().position(|l| l.starts_with('!')).unwrap();
        assert!(lines[..incl_start].iter().all(|l| !l.starts_with('!')));
        assert!(lines[incl_start..].iter().all(|l| l.starts_with('!')));
        assert!(lines.contains(&"!CLAUDE.md".to_string()));
        assert!(lines.contains(&"projects/**/sessions/**".to_string()));
    }
}
