//! Single-instance daemon advisory locking and PID file handling.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ProcessLock {
    _file: File,
    lock_path: PathBuf,
    pid_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessLockError {
    #[error("process is already running (pid {0:?})")]
    AlreadyRunning(Option<u32>),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl ProcessLock {
    /// Acquire an exclusive non-blocking advisory lock on `dir/daemon.lock` and write `dir/daemon.pid`.
    pub fn acquire(dir: &Path) -> Result<Self, ProcessLockError> {
        std::fs::create_dir_all(dir)?;
        let lock_path = dir.join("daemon.lock");
        let pid_path = dir.join("daemon.pid");

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
                    let existing_pid = std::fs::read_to_string(&pid_path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    return Err(ProcessLockError::AlreadyRunning(existing_pid));
                }
                return Err(ProcessLockError::Io(err));
            }
        }

        let pid = std::process::id();
        let _ = std::fs::write(&pid_path, format!("{pid}\n"));

        Ok(Self {
            _file: file,
            lock_path,
            pid_path,
        })
    }
}

impl Drop for ProcessLock {
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
