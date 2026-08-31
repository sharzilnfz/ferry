use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pin_error::{io_at, PinError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldChunk {
    pub id: String,

    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldEntry {
    pub held_sec: i64,
    pub held_nsec: u32,

    pub path: String,

    pub device_id: String,

    pub remote_manifest_id: String,

    pub chunks: Vec<HeldChunk>,

    pub decision: String,

    pub conflict_winner: Option<String>,
}

pub fn distinct_paths(entries: &[HeldEntry]) -> Vec<String> {
    let mut out: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Clone, Debug)]
pub struct HeldLedger {
    dir: PathBuf,
}

impl HeldLedger {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        HeldLedger {
            dir: state_dir.into().join("held"),
        }
    }

    fn path_for(&self, peer_hex: &str) -> PathBuf {
        self.dir.join(format!("{peer_hex}.jsonl"))
    }

    pub fn append(&self, peer_hex: &str, entries: &[HeldEntry]) -> Result<(), PinError> {
        if entries.is_empty() {
            return Ok(());
        }
        let existing = self.load_peer(peer_hex)?;
        let mut seen: std::collections::BTreeSet<(&str, &str)> = existing
            .iter()
            .map(|e| (e.path.as_str(), e.remote_manifest_id.as_str()))
            .collect();
        let mut to_append = Vec::new();
        for e in entries {
            if seen.insert((e.path.as_str(), e.remote_manifest_id.as_str())) {
                to_append.push(e);
            }
        }
        if to_append.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| io_at(&self.dir, e))?;
        let mut body = String::new();
        for e in to_append {
            body.push_str(&serde_json::to_string(e).expect("held entry serializes"));
            body.push('\n');
        }
        use std::io::Write;
        let path = self.path_for(peer_hex);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io_at(&path, e))?;
        f.write_all(body.as_bytes()).map_err(|e| io_at(&path, e))?;
        f.sync_all().map_err(|e| io_at(&path, e))?;
        Ok(())
    }

    pub fn load_peer(&self, peer_hex: &str) -> Result<Vec<HeldEntry>, PinError> {
        let path = self.path_for(peer_hex);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at(&path, e)),
        };
        let torn_tail = !text.is_empty() && !text.ends_with('\n');
        let lines: Vec<&str> = text.lines().collect();
        let last = lines.len().saturating_sub(1);
        let mut out = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed = serde_json::from_str::<HeldEntry>(line);
            match (parsed, i == last && torn_tail) {
                (Ok(e), _) => out.push(e),

                (Err(_), true) => break,
                (Err(e), false) => {
                    return Err(PinError::LedgerCorrupt {
                        path,
                        line: i + 1,
                        reason: e.to_string(),
                    })
                }
            }
        }
        Ok(out)
    }

    pub fn peers(&self) -> Result<Vec<String>, PinError> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at(&self.dir, e)),
        };
        let mut names = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn clear_peer(&self, peer_hex: &str) -> Result<bool, PinError> {
        let path = self.path_for(peer_hex);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_at(path, e)),
        }
    }
}

pub fn ledger_path(state_dir: &Path, peer_hex: &str) -> PathBuf {
    state_dir.join("held").join(format!("{peer_hex}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> HeldEntry {
        entry_with_manifest(path, &"cc".repeat(32))
    }

    fn entry_with_manifest(path: &str, man_hex: &str) -> HeldEntry {
        HeldEntry {
            held_sec: 1_787_574_000,
            held_nsec: 5,
            path: path.into(),
            device_id: "bb".repeat(32),
            remote_manifest_id: man_hex.into(),
            chunks: vec![HeldChunk {
                id: "dd".repeat(32),
                len: 7,
            }],
            decision: "conflict".into(),
            conflict_winner: Some("local".into()),
        }
    }

    #[test]
    fn append_load_roundtrip_and_distinct_paths() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = HeldLedger::new(dir.path());
        assert!(ledger.load_peer("ab").unwrap().is_empty());

        ledger
            .append(
                "ab",
                &[
                    entry_with_manifest("src/b.rs", &"c1".repeat(32)),
                    entry_with_manifest("src/a.rs", &"c1".repeat(32)),
                    entry_with_manifest("src/b.rs", &"c2".repeat(32)),
                ],
            )
            .unwrap();
        let got = ledger.load_peer("ab").unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].path, "src/b.rs");
        assert_eq!(
            distinct_paths(&got),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
        assert_eq!(ledger.peers().unwrap(), vec!["ab".to_string()]);

        ledger
            .append("ab", &[entry_with_manifest("src/a.rs", &"c1".repeat(32))])
            .unwrap();
        let got2 = ledger.load_peer("ab").unwrap();
        assert_eq!(got2.len(), 3);
    }

    #[test]
    fn empty_append_is_a_noop_clear_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = HeldLedger::new(dir.path());
        ledger.append("ab", &[]).unwrap();
        assert_eq!(ledger.peers().unwrap(), Vec::<String>::new());

        ledger.append("ab", &[entry("x")]).unwrap();
        assert!(ledger.clear_peer("ab").unwrap());
        assert!(!ledger.clear_peer("ab").unwrap(), "second clear is false");
        assert!(ledger.load_peer("ab").unwrap().is_empty());
    }

    #[test]
    fn torn_final_line_from_a_crash_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = HeldLedger::new(dir.path());
        ledger.append("p", &[entry("a.rs"), entry("b.rs")]).unwrap();

        let path = ledger_path(dir.path(), "p");
        let full = std::fs::read_to_string(&path).unwrap();
        let first_line_end = full.find('\n').unwrap() + 1;
        let torn = format!(
            "{}{{\"held_sec\":1,\"path\":\"half-writ",
            &full[..first_line_end]
        );
        std::fs::write(&path, &torn).unwrap();

        let got = ledger.load_peer("p").unwrap();
        assert_eq!(got.len(), 1, "torn tail dropped, complete line kept");
        assert_eq!(got[0].path, "a.rs");

        std::fs::write(&path, format!("{}\n", torn.trim_end())).unwrap();
        assert!(matches!(
            ledger.load_peer("p"),
            Err(PinError::LedgerCorrupt { .. })
        ));
    }
}
