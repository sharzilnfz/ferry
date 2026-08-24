//! The held-set ledgers: `.ferry/held/<peer-hex>.jsonl`.
//!
//! While a pin holds a peer's change, one line records it — path, the
//! remote manifest that carried the change, and the blob refs needed to
//! apply or quarantine it later (the chunks ride the normal fetch phase, so
//! release works even if the peer vanishes overnight). One file per peer,
//! append-only until a successful release clears it.
//!
//! Line format (one JSON object per held decision):
//!
//! ```json
//! {"held_sec":…,"held_nsec":…,"path":"src/main.rs","device_id":"<peer>",
//!  "remote_manifest_id":"<64 hex>","chunks":[{"id":"…","len":123}],
//!  "decision":"remote_apply|remote_delete|conflict",
//!  "conflict_winner":null|"local"|"remote"}
//! ```
//!
//! Crash safety: appends are single write(2)s of the whole batch followed
//! by a flush. A kill -9 mid-append can leave a torn FINAL line; readers
//! tolerate exactly that (a malformed last line in a file not ending in a
//! newline), anything else is loud corruption.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_at, PinError};

/// One chunk reference in a held entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldChunk {
    /// Chunk id, 64 lowercase hex.
    pub id: String,
    /// Plaintext length.
    pub len: u64,
}

/// One held remote change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldEntry {
    pub held_sec: i64,
    pub held_nsec: u32,
    /// Stored path, '/'-joined.
    pub path: String,
    /// The peer whose change this is (64 lowercase hex).
    pub device_id: String,
    /// The peer manifest this change arrived in.
    pub remote_manifest_id: String,
    /// Blob refs for the held version's bytes (empty for deletions).
    pub chunks: Vec<HeldChunk>,
    /// What the plan would have done without the pin:
    /// `remote_apply` | `remote_delete` | `conflict`.
    pub decision: String,
    /// For `conflict` only: which side three-way picks as winner.
    pub conflict_winner: Option<String>,
}

/// Distinct held paths across entries, sorted (for status surfaces).
pub fn distinct_paths(entries: &[HeldEntry]) -> Vec<String> {
    let mut out: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    out.sort();
    out.dedup();
    out
}

/// Filesystem home of the per-peer ledgers for one folder.
#[derive(Clone, Debug)]
pub struct HeldLedger {
    dir: PathBuf,
}

impl HeldLedger {
    /// `state_dir` is the folder's `.ferry` directory; ledgers live under
    /// `<state_dir>/held/`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        HeldLedger {
            dir: state_dir.into().join("held"),
        }
    }

    fn path_for(&self, peer_hex: &str) -> PathBuf {
        self.dir.join(format!("{peer_hex}.jsonl"))
    }

    /// Append a batch of entries for one peer. Creates the directory on
    /// first use. One write call + sync so partial batches cannot interleave.
    pub fn append(&self, peer_hex: &str, entries: &[HeldEntry]) -> Result<(), PinError> {
        if entries.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| io_at(&self.dir, e))?;
        let mut body = String::new();
        for e in entries {
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

    /// Load one peer's full ledger, oldest first. Tolerates a torn final
    /// line (crash mid-append); anything else corrupts loudly.
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
                // Crash mid-write: drop the incomplete tail, keep the rest.
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

    /// Every peer with a ledger file (hex names), sorted.
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

    /// Clear one peer's ledger (after a successful release). Returns
    /// whether a file existed.
    pub fn clear_peer(&self, peer_hex: &str) -> Result<bool, PinError> {
        let path = self.path_for(peer_hex);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_at(path, e)),
        }
    }
}

/// Path of one peer's ledger (for CLI output / docs).
pub fn ledger_path(state_dir: &Path, peer_hex: &str) -> PathBuf {
    state_dir.join("held").join(format!("{peer_hex}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> HeldEntry {
        HeldEntry {
            held_sec: 1_787_574_000,
            held_nsec: 5,
            path: path.into(),
            device_id: "bb".repeat(32),
            remote_manifest_id: "cc".repeat(32),
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
                &[entry("src/b.rs"), entry("src/a.rs"), entry("src/b.rs")],
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

        // Simulate kill -9 mid-append: complete first line, then a torn
        // second line WITHOUT the trailing newline.
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

        // The same garbage WITH a trailing newline (or mid-file) is loud.
        std::fs::write(&path, format!("{}\n", torn.trim_end())).unwrap();
        assert!(matches!(
            ledger.load_peer("p"),
            Err(PinError::LedgerCorrupt { .. })
        ));
    }
}
