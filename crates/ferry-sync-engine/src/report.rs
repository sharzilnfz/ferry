//! Structured conflict report: `.ferry/conflicts.jsonl`.
//!
//! One JSON object per line, machine-parseable and human-greppable:
//!
//! ```json
//! {"ts":"2026-08-24T12:00:00Z","folder_id":"<32 hex>","path":"a/b.txt",
//!  "kind":"both_changed","winner":{"device":"<64 hex>","mtime_sec":123,
//!  "mtime_nsec":4},"loser":{"device":"<64 hex>","mtime_sec":null,
//!  "mtime_nsec":null},"quarantined_as":"a/b.txt.ferry-conflict.ab12cd34-20260101-090000"}
//! ```
//!
//! `kind` is one of `both_changed`, `delete_vs_edit`, `add_vs_add`.
//! `mtime_*` are `null` for the deleting side of a delete-vs-edit (a
//! deletion has no mtime). `quarantined_as` is the stored relative path of
//! the loser copy, or `null` when nothing was quarantined (resurrections).
//!
//! The reader parses strictly and reports the line number on garbage: a
//! corrupt report must be noticed, not silently truncated.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One side of a conflict, as it appears in report lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStamp {
    pub device: String,
    pub mtime_sec: Option<i64>,
    pub mtime_nsec: Option<u32>,
}

/// One resolved conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEntry {
    /// RFC 3339 UTC wall clock at append time (second precision).
    pub ts: String,
    pub folder_id: String,
    pub path: String,
    pub kind: String,
    pub winner: DeviceStamp,
    pub loser: DeviceStamp,
    pub quarantined_as: Option<String>,
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("conflict log at {path} is corrupt at line {line}: {reason}")]
    Corrupt {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("io failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> LogError {
    LogError::Io {
        path: path.into(),
        source: e,
    }
}

pub fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("conflicts.jsonl")
}

/// Append entries, one JSON object per line. Creates the file and its
/// parent directory on first use.
pub fn append_entries(state_dir: &Path, entries: &[ConflictEntry]) -> Result<(), LogError> {
    if entries.is_empty() {
        return Ok(());
    }
    let path = log_path(state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_at(&parent, e))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| io_at(&path, e))?;
    for e in entries {
        let mut line = serde_json::to_string(e)
            .map_err(|ser| LogError::Corrupt {
                path: path.clone(),
                line: 0,
                reason: ser.to_string(),
            })?
            .into_bytes();
        line.push(b'\n');
        f.write_all(&line).map_err(|e| io_at(&path, e))?;
    }
    Ok(())
}

/// Read every recorded conflict, oldest first. Blank lines are skipped;
/// anything unparseable is a loud error carrying the line number.
pub fn list_conflicts(state_dir: &Path) -> Result<Vec<ConflictEntry>, LogError> {
    let path = log_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_at(path, e)),
    };
    let text = String::from_utf8(bytes).map_err(|_| LogError::Corrupt {
        path: path.clone(),
        line: 0,
        reason: "not valid UTF-8".to_string(),
    })?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: ConflictEntry = serde_json::from_str(line).map_err(|e| LogError::Corrupt {
            reason: e.to_string(),
            path: path.clone(),
            line: idx + 1,
        })?;
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(dev: &str, sec: Option<i64>) -> DeviceStamp {
        DeviceStamp {
            device: dev.to_string(),
            mtime_sec: sec,
            mtime_nsec: sec.map(|_| 5),
        }
    }

    fn entry(path: &str, kind: &str, quarantined: Option<&str>) -> ConflictEntry {
        ConflictEntry {
            ts: "2026-08-24T10:00:00Z".to_string(),
            folder_id: "aa".repeat(16),
            path: path.to_string(),
            kind: kind.to_string(),
            winner: stamp("bb".repeat(32).as_str(), Some(100)),
            loser: stamp("cc".repeat(32).as_str(), if kind == "delete_vs_edit" { None } else { Some(90) }),
            quarantined_as: quarantined.map(str::to_string),
        }
    }

    #[test]
    fn append_then_list_round_trips_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();
        let batch = vec![
            entry("f.txt", "both_changed", Some("f.txt.ferry-conflict.cccccccc-20260824-090000")),
            entry("g.txt", "delete_vs_edit", None),
            entry("h.txt", "add_vs_add", Some("h.txt.ferry-conflict.cccccccc-20260823-080000")),
        ];
        append_entries(sd, &batch).unwrap();
        // A second empty call appends nothing.
        append_entries(sd, &[]).unwrap();
        assert_eq!(list_conflicts(sd).unwrap(), batch);
    }

    #[test]
    fn listing_an_absent_log_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_conflicts(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn corrupt_lines_fail_loudly_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();
        append_entries(sd, &[entry("ok.txt", "both_changed", None)]).unwrap();
        let path = log_path(sd);
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("{\"ts\": not json\n");
        std::fs::write(&path, raw).unwrap();
        match list_conflicts(sd) {
            Err(LogError::Corrupt { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected loud corruption error, got {other:?}"),
        }
    }

    #[test]
    fn jsonl_shape_is_greppable_and_has_the_documented_keys() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();
        append_entries(sd, &[entry("f.txt", "both_changed", Some("q"))]).unwrap();
        let raw = std::fs::read_to_string(log_path(sd)).unwrap();
        assert!(raw.starts_with("{\"ts\":\"2026-"));
        for key in [
            "\"folder_id\"", "\"path\"", "\"kind\"", "\"winner\"", "\"loser\"",
            "\"quarantined_as\"", "\"mtime_sec\"", "\"device\"",
        ] {
            assert!(raw.contains(key), "missing {key}");
        }
        assert_eq!(raw.lines().count(), 1);
    }
}
