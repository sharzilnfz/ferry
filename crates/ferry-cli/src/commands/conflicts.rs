//! `ferry conflicts list`: read `.ferry/conflicts.jsonl` through
//! ferry-sync-engine and render a table or a JSON array.

use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

pub fn run(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let entries = ferry_sync_engine::list_conflicts(&opened.state_dir()).map_err(|e| {
        CliError::new(
            "conflict-log",
            e.to_string(),
            "the conflict report is corrupt; fix or archive .ferry/conflicts.jsonl (entries are never lost silently)",
        )
    })?;

    let json_doc = json!({
        "command": "conflicts",
        "folder": opened.root.display().to_string(),
        "entries": entries,
    });

    let mut human = String::new();
    if entries.is_empty() {
        human.push_str("No conflicts recorded.\n");
    } else {
        human.push_str(&format!(
            "{:<20} {:<14} {:<28} {}\n",
            "WHEN", "KIND", "PATH", "QUARANTINED AS"
        ));
        for e in &entries {
            human.push_str(&format!(
                "{:<20} {:<14} {:<28} {}\n",
                e.ts,
                e.kind,
                truncate(&e.path, 28),
                e.quarantined_as.as_deref().unwrap_or("-")
            ));
        }
        human.push_str(&format!("{} conflict(s) total.\n", entries.len()));
    }

    Ok(Output::new(json_doc, human))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max - 1;
        format!("{}…", s.chars().take(keep).collect::<String>())
    }
}
