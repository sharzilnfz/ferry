use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStamp {
    pub device: String,
    pub mtime_sec: Option<i64>,
    pub mtime_nsec: Option<u32>,
}

impl DeviceStamp {
    pub(crate) fn new(device: [u8; 32], mtime: Option<(i64, u32)>) -> Self {
        DeviceStamp {
            device: ferry_store::format::hex(&device),
            mtime_sec: mtime.map(|m| m.0),
            mtime_nsec: mtime.map(|m| m.1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEntry {
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

pub const COMPACT_MAX_LINES: usize = 4096;
pub const COMPACT_KEEP_LINES: usize = 1024;

pub fn append_entries(state_dir: &Path, entries: &[ConflictEntry]) -> Result<(), LogError> {
    if entries.is_empty() {
        return Ok(());
    }
    let path = log_path(state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
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
    drop(f);
    compact_if_needed(&path)
}

fn compact_if_needed(path: &Path) -> Result<(), LogError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Err(io_at(path, e)),
    };
    let text = String::from_utf8(bytes).map_err(|_| LogError::Corrupt {
        path: path.to_path_buf(),
        line: 0,
        reason: "not valid UTF-8".to_string(),
    })?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= COMPACT_MAX_LINES {
        return Ok(());
    }
    let kept = &lines[lines.len() - COMPACT_KEEP_LINES..];
    let mut body = kept.join("\n");
    body.push('\n');

    let tmp = path.with_file_name("conflicts.jsonl.tmp.compacting");
    std::fs::write(&tmp, body.as_bytes()).map_err(|e| io_at(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_at(path, e))?;
    Ok(())
}

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

    fn stamp(dev_hex: &str, sec: Option<i64>) -> DeviceStamp {
        DeviceStamp::new(
            ferry_store::format::unhex::<32>(dev_hex).unwrap(),
            sec.map(|s| (s, 5)),
        )
    }

    fn entry(path: &str, kind: &str, quarantined: Option<&str>) -> ConflictEntry {
        ConflictEntry {
            ts: "2026-08-24T10:00:00Z".to_string(),
            folder_id: "aa".repeat(16),
            path: path.to_string(),
            kind: kind.to_string(),
            winner: stamp("bb".repeat(32).as_str(), Some(100)),
            loser: stamp(
                "cc".repeat(32).as_str(),
                if kind == "delete_vs_edit" {
                    None
                } else {
                    Some(90)
                },
            ),
            quarantined_as: quarantined.map(str::to_string),
        }
    }

    #[test]
    fn append_then_list_round_trips_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();
        let batch = vec![
            entry(
                "f.txt",
                "both_changed",
                Some("f.txt.ferry-conflict.cccccccc-20260824-090000"),
            ),
            entry("g.txt", "delete_vs_edit", None),
            entry(
                "h.txt",
                "add_vs_add",
                Some("h.txt.ferry-conflict.cccccccc-20260823-080000"),
            ),
        ];
        append_entries(sd, &batch).unwrap();

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
    fn append_compacts_on_threshold_keeping_the_newest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();

        let path = log_path(sd);
        let mut raw = String::new();
        for i in 0..COMPACT_MAX_LINES {
            raw.push_str(
                &serde_json::to_string(&entry(&format!("f{i}.txt"), "both_changed", None)).unwrap(),
            );
            raw.push('\n');
        }
        std::fs::create_dir_all(sd).unwrap();
        std::fs::write(&path, &raw).unwrap();

        append_entries(sd, &[entry("newest.txt", "add_vs_add", None)]).unwrap();

        let listed = list_conflicts(sd).unwrap();
        assert_eq!(listed.len(), COMPACT_KEEP_LINES);
        assert_eq!(listed.last().unwrap().path, "newest.txt", "newest survives");
        assert_eq!(
            listed.first().unwrap().path,
            format!("f{}.txt", COMPACT_MAX_LINES + 1 - COMPACT_KEEP_LINES),
            "oldest dropped, middle kept in order"
        );
        assert!(
            !path
                .with_file_name("conflicts.jsonl.tmp.compacting")
                .exists(),
            "compaction temp is renamed away"
        );

        append_entries(sd, &[entry("again.txt", "both_changed", None)]).unwrap();
        assert_eq!(list_conflicts(sd).unwrap().len(), COMPACT_KEEP_LINES + 1);
    }

    #[test]
    fn jsonl_shape_is_greppable_and_has_the_documented_keys() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path();
        append_entries(sd, &[entry("f.txt", "both_changed", Some("q"))]).unwrap();
        let raw = std::fs::read_to_string(log_path(sd)).unwrap();
        assert!(raw.starts_with("{\"ts\":\"2026-"));
        for key in [
            "\"folder_id\"",
            "\"path\"",
            "\"kind\"",
            "\"winner\"",
            "\"loser\"",
            "\"quarantined_as\"",
            "\"mtime_sec\"",
            "\"device\"",
        ] {
            assert!(raw.contains(key), "missing {key}");
        }
        assert_eq!(raw.lines().count(), 1);
    }
}
