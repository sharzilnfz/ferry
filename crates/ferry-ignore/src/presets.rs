#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preset {
    pub id: String,

    pub description: String,

    pub includes: Vec<String>,

    pub excludes: Vec<String>,
}

impl Preset {
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
                "node_modules/".into(),
            ],
        }
    }

    pub fn builtin(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Preset::claude()),
            "opencode" => Some(Preset::opencode()),
            _ => None,
        }
    }

    pub fn rule_lines(&self) -> Vec<String> {
        self.excludes
            .iter()
            .cloned()
            .chain(self.includes.iter().map(|i| format!("!{i}")))
            .collect()
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("preset serializes")
    }

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
