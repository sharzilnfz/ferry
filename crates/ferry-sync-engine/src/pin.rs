























use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pin_error::{io_at, PinError};


pub const PIN_FORMAT_VERSION: u32 = 1;


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinRecord {
    pub format_version: u32,
    
    pub device_id: String,
    
    pub pid: u32,
    pub started_sec: i64,
    pub started_nsec: u32,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_sec: Option<i64>,
    
    pub paths: Vec<String>,
    
    pub released: bool,
    
    
    
    #[serde(default)]
    pub base_agreements: BTreeMap<String, String>,
    
    
    
    
    
    #[serde(default)]
    pub proc_start_token: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    
    Alive,
    
    Stale,
}

impl PinRecord {
    pub fn liveness(&self) -> Liveness {
        if self.pid == 0 {
            return Liveness::Alive; 
        }
        match self.proc_start_token {
            Some(stamped) => match ferry_platform::process_start_token(self.pid) {
                Some(actual) if actual == stamped => Liveness::Alive,
                Some(_) => Liveness::Stale,
                None if pid_alive(self.pid) => Liveness::Alive,
                None => Liveness::Stale,
            },
            None if pid_alive(self.pid) => Liveness::Alive,
            None => Liveness::Stale,
        }
    }

    
    
    pub fn holding(&self) -> bool {
        if self.released {
            return false;
        }
        if let Some(exp) = self.expires_sec {
            if ferry_platform::time::now_unix().0 >= exp {
                return false;
            }
        }
        self.liveness() == Liveness::Alive
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return true; 
    }
    #[cfg(unix)]
    {
        let Ok(signed) = libc::pid_t::try_from(pid) else {
            return false;
        };
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

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_ACCESS_DENIED as i32);
        }
        let mut exit_code: u32 = 0;
        let queried = unsafe { GetExitCodeProcess(handle, std::ptr::from_mut(&mut exit_code)) };
        unsafe { CloseHandle(handle) };
        if queried == 0 {
            return false;
        }
        exit_code == STILL_ACTIVE
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}


#[derive(Clone, Debug)]
pub struct PinStore {
    path: PathBuf,
}

impl PinStore {
    pub const FILE_NAME: &str = "pin-state.json";

    
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        PinStore {
            path: state_dir.into().join(Self::FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    
    
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

    
    pub fn start(&self, rec: &PinRecord) -> Result<(), PinError> {
        if let Some(existing) = self.load()? {
            if existing.holding() {
                return Err(PinError::PinActive { pid: existing.pid });
            }
        }
        let mut rec = rec.clone();
        rec.format_version = PIN_FORMAT_VERSION;
        if rec.pid == std::process::id() {
            rec.proc_start_token = ferry_platform::process_start_token(rec.pid);
        }
        let body = serde_json::to_string_pretty(&rec).expect("pin record serializes");
        let tmp = self.unique_tmp();
        self.ensure_dir()?;
        std::fs::write(&tmp, body).map_err(|e| io_at(&tmp, e))?;
        if let Some(existing) = self.load()? {
            if existing.holding() {
                let _ = std::fs::remove_file(&tmp);
                return Err(PinError::PinActive { pid: existing.pid });
            }
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| io_at(&self.path, e))?;
        Ok(())
    }

    
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
            expires_sec: None,
            paths: vec!["src/**".into()],
            released: false,
            base_agreements: BTreeMap::new(),
            proc_start_token: None,
        }
    }

    #[test]
    fn pin_expiration_stops_holding() {
        let mut rec = record(std::process::id());
        rec.proc_start_token = ferry_platform::process_start_token(rec.pid);
        let now = ferry_platform::time::now_unix().0;

        rec.expires_sec = Some(now + 3600);
        assert!(rec.holding());

        rec.expires_sec = Some(now - 10);
        assert!(!rec.holding());
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
        let mut child = ferry_platform::spawn_sleeper(30).expect("spawn sleeper");
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
}
