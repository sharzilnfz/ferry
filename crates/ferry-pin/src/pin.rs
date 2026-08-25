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
//! process exists under another owner) and OpenProcess+GetExitCodeProcess
//! on windows (open failure with ACCESS_DENIED counts as alive for the
//! same reason; exit code 259 STILL_ACTIVE means alive). pid 0 means
//! "unknown" and is treated as alive so tests degrade to active.
//!
//! Existence alone is not enough (T-06): pids are recycled, so a dead
//! agent's pin would go immortal as soon as some unrelated process
//! inherited its pid. [`PinStore::start`] therefore stamps the writer's
//! PROCESS START TIME ([`ferry_platform::process_start_token`]) whenever
//! the writer is this process, and liveness requires the pid's current
//! occupant to have that same birth time — a mismatch is pid reuse and
//! reads STALE. Records written before T-06 lack the stamp; per the
//! tolerant-reader rule they keep working under existence-only liveness.

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
    /// Opaque start-time identity of `pid`'s process instance, stamped by
    /// [`PinStore::start`] when the declared writer is THIS process (T-06).
    /// Whatever later reuses the pid carries a different value, so pid
    /// reuse reads STALE instead of immortal. Absent in pre-T-06 records;
    /// those degrade to existence-only liveness.
    #[serde(default)]
    pub proc_start_token: Option<u64>,
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
        if self.pid == 0 {
            return Liveness::Alive; // "unknown writer" degrades to active
        }
        match self.proc_start_token {
            Some(stamped) => match ferry_platform::process_start_token(self.pid) {
                // The pid's CURRENT occupant was born when the pin says its
                // writer did: same process instance.
                Some(actual) if actual == stamped => Liveness::Alive,
                // The pid runs but belongs to a LATER process: pid reuse.
                Some(_) => Liveness::Stale,
                // Start times uninspectable on this platform: existence
                // probe only, exactly like pre-T-06 records.
                None if pid_alive(self.pid) => Liveness::Alive,
                None => Liveness::Stale,
            },
            // Pre-T-06 record without a stamp: existence-only liveness.
            None if pid_alive(self.pid) => Liveness::Alive,
            None => Liveness::Stale,
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
        // Pids beyond pid_t range cannot exist on this kernel. Casting
        // anyway would sign-flip the value into a process-group target
        // (kill(-n) signals a GROUP); refuse instead.
        let Ok(signed) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // Safety: kill(2) with signal 0 is a pure existence probe.
        let rc = unsafe { libc::kill(signed, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;

        // Safety: a query-only handle; never used to signal or terminate,
        // and closed on every path before returning.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // Access denied mirrors unix EPERM: a process we are not allowed
            // to inspect might be alive, so count it as alive. Any other
            // open failure (invalid pid, reaped process) means gone.
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_ACCESS_DENIED as i32);
        }
        let mut exit_code: u32 = 0;
        let queried = unsafe { GetExitCodeProcess(handle, std::ptr::from_mut(&mut exit_code)) };
        unsafe { CloseHandle(handle) };
        if queried == 0 {
            return false; // vanished between open and query
        }
        // Known caveat: a process that exited WITH code 259 reads as alive.
        // Same class of imprecision as unix EPERM-means-alive; acceptable
        // for stale-pin surfacing (a wrong 'alive' heals on release).
        exit_code == STILL_ACTIVE
    }
    #[cfg(not(any(unix, windows)))]
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
    ///
    /// Concurrency (T-06): temp files carry unique pid+counter+nanos names,
    /// so concurrent starters cannot clobber one another's staging file;
    /// the final rename is atomic, so exactly ONE record wins the file.
    /// The active-pin check re-runs AFTER serialization to shrink the
    /// check-to-rename window.
    pub fn start(&self, rec: &PinRecord) -> Result<(), PinError> {
        if let Some(existing) = self.load()? {
            if existing.holding() {
                return Err(PinError::PinActive { pid: existing.pid });
            }
        }
        let mut rec = rec.clone();
        rec.format_version = PIN_FORMAT_VERSION;
        // Stamp liveness evidence when THIS process is the declared writer
        // (T-06): without it, pid reuse would keep the pin alive forever.
        if rec.pid == std::process::id() {
            rec.proc_start_token = ferry_platform::process_start_token(rec.pid);
        }
        let body = serde_json::to_string_pretty(&rec).expect("pin record serializes");
        let tmp = self.unique_tmp();
        self.ensure_dir()?;
        std::fs::write(&tmp, body).map_err(|e| io_at(&tmp, e))?;
        // Re-check under the staged write: another starter may have won the
        // file between the first load and now. Losing this race is clean —
        // our temp is removed, theirs stands.
        if let Some(existing) = self.load()? {
            if existing.holding() {
                let _ = std::fs::remove_file(&tmp);
                return Err(PinError::PinActive { pid: existing.pid });
            }
        }
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
        let tmp = self.unique_tmp();
        self.ensure_dir()?;
        std::fs::write(&tmp, body).map_err(|e| io_at(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io_at(&self.path, e))?;
        Ok(true)
    }

    /// A collision-free staging name beside the pin file: fixed names made
    /// concurrent writers overwrite each other's temp mid-write (T-06).
    /// pid + monotonic counter + clock nanos; same-process collisions need
    /// all three to collide, cross-process ones need the pid to repeat.
    fn unique_tmp(&self) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nsec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        self.path
            .with_extension(format!("json.tmp.{}.{}.{}", std::process::id(), seq, nsec))
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
            proc_start_token: None,
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

    #[test]
    fn start_stamps_the_current_process_start_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        let mut rec = record(std::process::id());
        rec.proc_start_token = None; // as callers construct it
        store.start(&rec).unwrap();

        let loaded = store.load().unwrap().expect("record exists");
        let stamped = loaded
            .proc_start_token
            .expect("start stamps its own writer");
        assert_eq!(
            ferry_platform::process_start_token(std::process::id()),
            Some(stamped),
            "stamp is THIS process's birth time"
        );
        assert_eq!(loaded.liveness(), Liveness::Alive);
        assert!(loaded.holding());

        // Round trip through the tolerant reader: a pre-T-06 record without
        // the field still loads and keeps working (existence-only).
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let dead = {
            child.kill().expect("kill -9 equivalent");
            child.wait().expect("reap");
            child.id()
        };
        std::fs::write(store.path(), serde_json::to_string(&record(dead)).unwrap()).unwrap();
        let legacy = store.load().unwrap().expect("legacy parses");
        assert_eq!(legacy.proc_start_token, None, "absent field defaults");
        assert!(
            !legacy.holding(),
            "existence-only liveness still expires dead writers"
        );
    }

    /// Granularity caveat (Linux): start-time ticks at `CONFIG_HZ` (~100/s).
    /// If this test binary is younger than one jiffy when the child spawns,
    /// both births land in the same tick and the tokens legitimately
    /// collide. One retry after a sleep past that window makes the
    /// assertion deterministic without weakening it (same pattern as the
    /// platform probe test in ferry-platform/src/procs.rs).
    #[test]
    fn pid_reuse_is_detected_through_start_time_mismatch() {
        let mut attempt = 0;
        let (child_pid, child_token, mut child) = loop {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleeper");
            let child_pid = child.id();
            let child_token = ferry_platform::process_start_token(child_pid)
                .expect("child start time inspectable while it runs");
            let ours = ferry_platform::process_start_token(std::process::id())
                .expect("own start time inspectable");
            if child_token == ours && attempt == 0 {
                // Same-tick birth collision; once we're older than a jiffy
                // any new child is provably born after us.
                child.kill().expect("kill sleeper");
                child.wait().expect("reap");
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
                continue;
            }
            break (child_pid, child_token, child);
        };

        let mut rec = record(child_pid);
        rec.proc_start_token = Some(child_token);
        assert_eq!(rec.liveness(), Liveness::Alive);
        assert!(rec.holding());

        // Simulate pid reuse WITHOUT waiting for one: the pid now belongs
        // to a different instance — model it with OUR birth time under the
        // child's pid. Existence would say alive; the mismatch says stale.
        let mut reused = record(child_pid);
        reused.proc_start_token =
            Some(ferry_platform::process_start_token(std::process::id()).unwrap());
        assert_ne!(
            reused.proc_start_token, rec.proc_start_token,
            "distinct instances must carry distinct tokens"
        );
        assert_eq!(reused.liveness(), Liveness::Stale, "mismatch => stale");
        assert!(!reused.holding());

        // The inverse fraud: an ALIVE pid recorded against a birth time it
        // never had (e.g. copied from another machine's file) is stale too.
        let mut forged = record(std::process::id());
        forged.proc_start_token =
            Some(ferry_platform::process_start_token(std::process::id()).unwrap() ^ 0xdead_beef);
        assert_eq!(forged.liveness(), Liveness::Stale);

        child.kill().expect("kill sleeper");
        child.wait().expect("reap");
        // Post-death convergence: on unix the reaped child is gone for real
        // (`kill(pid, 0)` fails even while our Child handle lingers), so an
        // honest record must flip to Stale. Poll briefly rather than assert
        // on the instant after reap.
        //
        // Skipped on Windows, where this assert is NONDETERMINISTIC at test
        // granularity, for two stacked platform reasons:
        // 1. While our `Child` handle is open (until scope end), the killed
        //    process OBJECT still exists and OpenProcess succeeds —
        //    GetProcessTimes keeps reporting the child's original birth
        //    time, so the token match legitimately reads Alive no matter
        //    how long we poll. Windows does not recycle a pid while any
        //    handle pins it, so dropping the handle is required before
        //    staleness is even observable.
        // 2. Once the handle closes, pids on Windows are recycled within
        //    milliseconds (multiples of 4) and cargo test spawns many
        //    processes. If the new occupant denies
        //    PROCESS_QUERY_LIMITED_INFORMATION, pid_alive's deliberate
        //    ACCESS_DENIED-means-alive rule (see the module docs) makes the
        //    record read Alive indefinitely. That is correct product
        //    behavior for an uninspectable squatter, but it means no poll
        //    bound can make this assertion deterministic here.
        // Windows still covers death-driven staleness deterministically:
        // stale_pin_detected_from_dead_pid_and_does_not_hold uses a
        // token-less record whose existence probe reads the pinned
        // terminated object's TerminateProcess exit code (1 ≠ STILL_ACTIVE),
        // and the synthetic `reused` / `forged` records above prove the
        // token-mismatch => Stale path on every platform.
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while rec.liveness() != Liveness::Stale {
                assert!(
                    std::time::Instant::now() < deadline,
                    "dead writer's record must go stale after death"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn pids_beyond_pid_t_range_are_stale_not_sign_flipped() {
        // u32::MAX used to cast to -1 on unix: kill(-1) signals EVERY
        // process group. It must read as gone, never as a probe target.
        let mut rec = record(u32::MAX);
        rec.proc_start_token = None;
        assert_eq!(rec.liveness(), Liveness::Stale);
        assert!(!rec.holding());
    }

    #[test]
    fn concurrent_starts_leave_exactly_one_valid_record() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PinStore::new(dir.path()));

        const RACERS: usize = 8;
        let results: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (1..=RACERS as u32)
                .map(|i| {
                    let store = Arc::clone(&store);
                    s.spawn(move || store.start(&record(i)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        // Every call either won or lost cleanly — never corrupted anything.
        assert_eq!(results.len(), RACERS);
        for r in &results {
            assert!(
                r.is_ok() || matches!(r, Err(PinError::PinActive { .. })),
                "{r:?}"
            );
        }

        // Exactly ONE valid record stands, and it is one of the submitted
        // ones (a whole document, never interleaved halves).
        let winner = store.load().unwrap().expect("exactly one parseable record");
        assert!(
            (1..=RACERS as u32).contains(&winner.pid),
            "winner pid {} is one of the racers",
            winner.pid
        );
        // No staging litter survives the race.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }
}
