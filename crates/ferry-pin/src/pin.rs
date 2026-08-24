//! The pinned-session record: `.ferry/pin-state.json`.
//!
//! Shape (schema in docs/cli-json.md, "Per-folder pin state"):
//!
//! ```json
//! {
//!   "format_version": 1,
//!   "device_id": "<64 hex>",          // who pins (this device)
//!   "pid": 4242,                       // writer process, for liveness
//!   "started_sec": 1787574896,
//!   "started_nsec": 0,
//!   "paths": ["src/**"],               // gitignore-style globs; ["*"] = all
//!   "released": false,
//!   "base_agreements": {"<peer-hex>": "<manifest-hex>"}
//! }                                     // last-agreed base per peer, frozen
//! ```                                   // at start; release's three-way base
//!
//! Crash safety: writes go through temp + rename inside `.ferry/`. A pin
//! whose `pid` no longer runs is STALE — surfaced by every command that
//! loads it (`holding() == false`, so nothing is held), but never deleted
//! behind the user's back: `ferry pin release` recovers the held set, or
//! `ferry pin stop` discards the marker deliberately. The file is only ever
//! removed by an explicit command or overwritten by a new `start`.
//!
//! pid liveness is `kill(pid, 0)` on unix (EPERM counts as alive: the
//! process exists under another owner). pid 0 means "unknown" and is
//! treated as alive so tests and non-unix platforms degrade to active.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_at, PinError};

/// Current `format_version` for [`PinRecord`].
pub const PIN_FORMAT_VERSION: u32 = 1;

/// One pinned session for one folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinRecord {
    pub format_version: u32,
    /// Device id (64 lowercase hex) of the pinning device.
    pub device_id: String,
    /// Process id of the declared writer (daemon/agent). Drives staleness.
    pub pid: u32,
    pub started_sec: i64,
    pub started_nsec: u32,
    /// Gitignore-style glob(s) scoping the hold; `["*"]` matches everything.
    pub paths: Vec<String>,
    /// True once stop/release ended the session.
    pub released: bool,
    /// Per-peer last-agreed manifest ids captured at pin START (peer hex →
    /// manifest hex). Release reconciles against these as the three-way
    /// base, exactly the "last-agreed before pin" ancestor.
    #[serde(default)]
    pub base_agreements: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// The recorded writer process still runs.
    Alive,
    /// The recorded writer process is gone (crash / kill -9 / reboot).
    Stale,
}

impl PinRecord {
    pub fn liveness(&self) -> Liveness {
        if pid_alive(self.pid) {
            Liveness::Alive
        } else {
            Liveness::Stale
        }
    }

    /// True while this record actually holds changes: unreleased AND its
    /// writer looks alive. Stale pins expire (nothing is held) but stay on
    /// disk until an explicit release/stop recovers or discards them.
    pub fn holding(&self) -> bool {
        !self.released && self.liveness() == Liveness::Alive
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return true; // "unknown writer" degrades to active
    }
    #[cfg(unix)]
    {
        // Safety: kill(2) with signal 0 is a pure existence probe.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Filesystem home of the pin record for one folder.
#[derive(Clone, Debug)]
pub struct PinStore {
    path: PathBuf,
}

impl PinStore {
    pub const FILE_NAME: &str = "pin-state.json";

    /// `state_dir` is the folder's `.ferry` directory.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        PinStore {
            path: state_dir.into().join(Self::FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the record. Absent file → `Ok(None)`. Present-but-wrong
    /// anything → loud [`PinError::Corrupt`], never a silent None.
    pub fn load(&self) -> Result<Option<PinRecord>, PinError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_at(&self.path, e)),
        };
        let rec: PinRecord = serde_json::from_str(&text).map_err(|e| PinError::Corrupt {
            path: self.path.clone(),
            reason: e.to_string(),
        })?;
        if rec.format_version != PIN_FORMAT_VERSION {
            return Err(PinError::Corrupt {
                path: self.path.clone(),
                reason: format!(
                    "format_version {} unsupported (this build understands {})",
                    rec.format_version, PIN_FORMAT_VERSION
                ),
            });
        }
        Ok(Some(rec))
    }

    /// Begin a session: write the record atomically. Refuses when an ACTIVE
    /// pin already exists ([`PinError::PinActive`]); a STALE pin is replaced
    /// (that replacement IS the recovery path after a crash).
    pub fn start(&self, rec: &PinRecord) -> Result<(), PinError> {
        if let Some(existing) = self.load()? {
            if existing.holding() {
                return Err(PinError::PinActive { pid: existing.pid });
            }
        }
        let mut rec = rec.clone();
        rec.format_version = PIN_FORMAT_VERSION;
        let body = serde_json::to_string_pretty(&rec).expect("pin record serializes");
        let tmp = self.path.with_extension("json.tmp");
        self.ensure_dir()?;
        std::fs::write(&tmp, body).map_err(|e| io_at(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io_at(&self.path, e))?;
        Ok(())
    }

    /// End the session: flip `released` to true in place (atomic rewrite).
    /// Returns whether a record existed. The file stays behind as history;
    /// a later `start` overwrites it.
    pub fn mark_released(&self) -> Result<bool, PinError> {
        let Some(mut rec) = self.load()? else {
            return Ok(false);
        };
        rec.released = true;
        let body = serde_json::to_string_pretty(&rec).expect("pin record serializes");
        let tmp = self.path.with_extension("json.tmp");
        self.ensure_dir()?;
        std::fs::write(&tmp, body).map_err(|e| io_at(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io_at(&self.path, e))?;
        Ok(true)
    }

    fn ensure_dir(&self) -> Result<(), PinError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32) -> PinRecord {
        PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: "aa".repeat(32),
            pid,
            started_sec: 1_787_574_000,
            started_nsec: 0,
            paths: vec!["src/**".into()],
            released: false,
            base_agreements: BTreeMap::new(),
        }
    }

    #[test]
    fn absent_state_loads_as_none_present_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        assert_eq!(store.load().unwrap(), None, "no pin yet");

        store.start(&record(std::process::id())).unwrap();
        let loaded = store.load().unwrap().expect("record exists");
        assert_eq!(loaded.paths, vec!["src/**".to_string()]);
        assert!(!loaded.released);
        assert_eq!(loaded.format_version, PIN_FORMAT_VERSION);
        assert!(store.path().is_file(), "lives at .ferry/pin-state.json");
    }

    #[test]
    fn second_start_refused_while_active_allowed_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        store.start(&record(std::process::id())).unwrap();

        let err = store.start(&record(1)).unwrap_err();
        assert!(matches!(err, PinError::PinActive { .. }), "{err}");

        store.mark_released().unwrap();
        store.start(&record(1)).unwrap();
        let now = store.load().unwrap().unwrap();
        assert_eq!(now.pid, 1, "new session replaced the released one");
        assert!(!now.released);
    }

    #[test]
    fn stale_pin_detected_from_dead_pid_and_does_not_hold() {
        // kill -9 simulation: a real child process, killed, its pid orphaned
        // into the record.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let pid = child.id();
        let dead = {
            child.kill().expect("kill -9 equivalent");
            child.wait().expect("reap");
            pid
        };

        let rec = record(dead);
        assert_eq!(rec.liveness(), Liveness::Stale);
        assert!(!rec.holding(), "a dead writer cannot hold changes");

        let live = record(std::process::id());
        assert_eq!(live.liveness(), Liveness::Alive);
        assert!(live.holding());
    }

    #[test]
    fn corrupt_or_future_format_is_a_loud_error_never_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        std::fs::write(store.path(), "{not json").unwrap();
        assert!(matches!(store.load(), Err(PinError::Corrupt { .. })));

        let mut future = record(1);
        future.format_version = 99;
        std::fs::write(store.path(), serde_json::to_string(&future).unwrap()).unwrap();
        assert!(matches!(store.load(), Err(PinError::Corrupt { .. })));
    }

    #[test]
    fn mark_released_flips_flag_and_reports_absence() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        assert!(!store.mark_released().unwrap(), "nothing to release");

        store.start(&record(std::process::id())).unwrap();
        assert!(store.mark_released().unwrap());
        let rec = store.load().unwrap().unwrap();
        assert!(rec.released);
        assert!(!rec.holding());
    }
}
