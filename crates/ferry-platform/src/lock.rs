







use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::procs::process_start_token;


pub const PID_FILENAME: &str = "daemon.pid";
const LOCK_FILENAME: &str = "daemon.lock";



pub const TERMINATE_DEADLINE: Duration = Duration::from_secs(5);



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    
    
    pub start_token: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonLockError {
    #[error("process is already running (pid {0:?})")]
    AlreadyRunning(Option<u32>),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateOutcome {
    
    Stopped { pid: u32 },
    
    NotRunning,
    
    Timeout { pid: u32 },
}

fn pid_path(dir: &Path) -> PathBuf {
    dir.join(PID_FILENAME)
}




pub fn read_pid(dir: &Path) -> Option<PidRecord> {
    let text = std::fs::read_to_string(pid_path(dir)).ok()?;
    let mut fields = text.split_whitespace();
    let pid = fields.next()?.parse::<u32>().ok()?;
    let start_token = fields.next().and_then(|t| t.parse::<u64>().ok());
    Some(PidRecord { pid, start_token })
}



pub fn running_pid(dir: &Path) -> Option<u32> {
    let record = read_pid(dir)?;
    instance_alive(&record).then_some(record.pid)
}


pub fn is_running(dir: &Path) -> bool {
    running_pid(dir).is_some()
}






pub fn terminate(dir: &Path, deadline: Duration) -> Result<TerminateOutcome, DaemonLockError> {
    let path = pid_path(dir);
    let Some(record) = read_pid(dir) else {
        
        let _ = std::fs::remove_file(&path);
        return Ok(TerminateOutcome::NotRunning);
    };

    signal_terminate(record.pid)?;

    let deadline = Instant::now() + deadline;
    let mut backoff = Duration::from_millis(50);
    loop {
        if !still_alive(&record) {
            let _ = std::fs::remove_file(&path);
            return Ok(TerminateOutcome::Stopped { pid: record.pid });
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(TerminateOutcome::Timeout { pid: record.pid });
        }
        std::thread::sleep(backoff.min(deadline - now));
        backoff = (backoff * 2).min(Duration::from_millis(400));
    }
}



fn instance_alive(record: &PidRecord) -> bool {
    match record.start_token {
        Some(recorded) => process_start_token(record.pid) == Some(recorded),
        None => process_start_token(record.pid).is_some(),
    }
}





#[cfg(unix)]
fn still_alive(record: &PidRecord) -> bool {
    let Ok(pid) = i32::try_from(record.pid) else {
        return instance_alive(record);
    };
    
    let rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    match rc {
        0 => true,
        rc if rc == pid => false,
        _ => instance_alive(record),
    }
}

#[cfg(not(unix))]
fn still_alive(record: &PidRecord) -> bool {
    instance_alive(record)
}

fn signal_terminate(pid: u32) -> Result<(), DaemonLockError> {
    #[cfg(unix)]
    {
        
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            
            
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err.into());
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            
            return Ok(());
        }
        let ok = unsafe { TerminateProcess(handle, 1) };
        
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}



#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
    lock_path: PathBuf,
    pid_path: PathBuf,
}

impl DaemonLock {
    
    
    pub fn acquire(dir: &Path) -> Result<Self, DaemonLockError> {
        std::fs::create_dir_all(dir)?;
        let lock_path = dir.join(LOCK_FILENAME);
        let path = pid_path(dir);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    let existing_pid = read_pid(dir).map(|record| record.pid);
                    return Err(DaemonLockError::AlreadyRunning(existing_pid));
                }
                return Err(DaemonLockError::Io(err));
            }
        }

        let pid = std::process::id();
        let stamp = match process_start_token(pid) {
            Some(token) => format!("{pid} {token}\n"),
            None => format!("{pid}\n"),
        };
        let _ = std::fs::write(&path, stamp);

        Ok(Self {
            _file: file,
            lock_path,
            pid_path: path,
        })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid_path);
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self._file.as_raw_fd();
            
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::spawn_sleeper;

    fn stamp(pid: u32, token: Option<u64>) -> String {
        match token {
            Some(token) => format!("{pid} {token}\n"),
            None => format!("{pid}\n"),
        }
    }

    
    
    fn live_record(dir: &Path) -> (std::process::Child, PidRecord) {
        let child = spawn_sleeper(30).expect("spawn sleeper");
        let token = process_start_token(child.id());
        std::fs::write(pid_path(dir), stamp(child.id(), token)).unwrap();
        let record = read_pid(dir).unwrap();
        assert!(is_running(dir), "freshly recorded live pid is running");
        (child, record)
    }

    fn reap(child: &mut std::process::Child) {
        child.kill().expect("kill sleeper");
        child.wait().expect("reap sleeper");
    }

    #[test]
    fn acquire_stamps_own_pid_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = DaemonLock::acquire(dir.path()).unwrap();
        let record = read_pid(dir.path()).expect("pid file written");
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.start_token, process_start_token(std::process::id()));
        assert!(is_running(dir.path()));
    }

    #[test]
    fn read_pid_parses_full_legacy_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(pid_path(dir.path()), "42 7\n").unwrap();
        assert_eq!(
            read_pid(dir.path()),
            Some(PidRecord {
                pid: 42,
                start_token: Some(7)
            })
        );

        std::fs::write(pid_path(dir.path()), "42\n").unwrap();
        assert_eq!(
            read_pid(dir.path()),
            Some(PidRecord {
                pid: 42,
                start_token: None
            })
        );

        std::fs::write(pid_path(dir.path()), "not a pid\n").unwrap();
        assert_eq!(read_pid(dir.path()), None);

        std::fs::remove_file(pid_path(dir.path())).unwrap();
        assert_eq!(read_pid(dir.path()), None);
    }

    #[test]
    fn is_running_false_without_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_running(dir.path()));
    }

    #[test]
    fn is_running_false_for_dead_pid() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_sleeper(30).expect("spawn sleeper");
        let token = process_start_token(child.id());
        reap(&mut child);
        std::fs::write(pid_path(dir.path()), stamp(child.id(), token)).unwrap();
        assert!(!is_running(dir.path()), "dead pid must read as stopped");
    }

    #[test]
    fn is_running_rejects_pid_reuse_with_foreign_token() {
        let dir = tempfile::tempdir().unwrap();
        let (mut child, record) = live_record(dir.path());
        
        
        let forged = PidRecord {
            start_token: record.start_token.map(|t| t.wrapping_add(1)),
            ..record
        };
        std::fs::write(pid_path(dir.path()), stamp(forged.pid, forged.start_token)).unwrap();
        assert!(
            !is_running(dir.path()),
            "a live pid with a foreign token is not our daemon"
        );
        reap(&mut child);
    }

    #[cfg(unix)]
    #[test]
    fn terminate_stops_process_and_unlinks_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let (child, record) = live_record(dir.path());

        let outcome = terminate(dir.path(), TERMINATE_DEADLINE).expect("terminate succeeds");

        assert_eq!(outcome, TerminateOutcome::Stopped { pid: record.pid });
        assert!(
            !pid_path(dir.path()).exists(),
            "pid file unlinked after exit"
        );
        assert!(!is_running(dir.path()));
        assert!(
            process_start_token(record.pid).is_none(),
            "OS confirms the process is gone"
        );
        drop(child);
    }

    #[cfg(unix)]
    #[test]
    fn terminate_timeout_preserves_pid_file_and_liveness() {
        let dir = tempfile::tempdir().unwrap();
        
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap \"\" TERM; sleep 30"])
            .spawn()
            .expect("spawn TERM-immune sleeper");
        std::thread::sleep(Duration::from_millis(100));
        let token = process_start_token(child.id());
        std::fs::write(pid_path(dir.path()), stamp(child.id(), token)).unwrap();

        let outcome = terminate(dir.path(), Duration::from_millis(300)).unwrap();

        assert_eq!(outcome, TerminateOutcome::Timeout { pid: child.id() });
        assert!(
            pid_path(dir.path()).is_file(),
            "pid file preserved on timeout"
        );
        assert!(is_running(dir.path()), "status still reports the live pid");

        reap(&mut child);
    }

    #[test]
    fn terminate_without_record_reports_not_running_and_clears_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            terminate(dir.path(), TERMINATE_DEADLINE).unwrap(),
            TerminateOutcome::NotRunning
        );

        std::fs::write(pid_path(dir.path()), "garbage\n").unwrap();
        assert_eq!(
            terminate(dir.path(), TERMINATE_DEADLINE).unwrap(),
            TerminateOutcome::NotRunning
        );
        assert!(!pid_path(dir.path()).exists(), "stale pid file cleared");
    }
}
