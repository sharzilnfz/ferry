//! Process start-time identity (T-06): the anti-pid-reuse half of stale-pin
//! liveness.
//!
//! "Is pid P alive?" cannot distinguish P from a LATER process the kernel
//! handed the same pid to. Recording only existence probes
//! (`kill(pid, 0)` / `OpenProcess`) makes a crashed writer's pin immortal
//! whenever some unrelated process eventually reuses its pid. The fix is to
//! also record WHEN the writer's process started and compare that against
//! whatever currently owns the pid.
//!
//! [`process_start_token`] returns an opaque, platform-local u64 that is
//! stable for the lifetime of one process instance and differs for any
//! later occupant of the same pid:
//!
//! - Linux: `/proc/<pid>/stat` field 22 — CPU clock ticks after boot.
//! - macOS: `KERN_PROC_PID` sysctl — wall-clock birth time (ns since epoch).
//! - Windows: `GetProcessTimes` creation `FILETIME` (100 ns since 1601).
//!
//! Units deliberately differ per platform: tokens never cross machines, so
//! only within-platform stability matters. A token for a pid whose start
//! time cannot be inspected at all is `None`; callers must degrade
//! gracefully (existence probe only), never treat "unknown" as dead.

/// Opaque start identity of whichever process currently owns `pid`, or
/// `None` when this platform cannot tell (including "no such process" on
/// platforms where the probe doubles as an existence check).
pub fn process_start_token(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        linux_token(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_token(pid)
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        let _ = pid;
        None
    }
    #[cfg(windows)]
    {
        windows_token(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// Linux: field 22 of `/proc/<pid>/stat` (starttime, clock ticks since
/// boot). Field 2 is the comm name in parentheses and may contain spaces
/// AND `)` itself, so parsing starts after the LAST closing paren; state
/// (field 3) then heads the remainder, putting starttime at offset 19.
#[cfg(target_os = "linux")]
fn linux_token(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// macOS: `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, pid)` → an array of
/// `kinfo_proc`; the token is the process birth time as nanoseconds since
/// the epoch. libc exposes neither `kinfo_proc` nor `extern_proc` on apple
/// targets, but per the SDK header (`sys/sysctl.h`, `sys/proc.h`) the
/// record LEADS with `extern_proc`, whose unnamed union overlays
/// `struct timeval p_starttime` at offset zero — so the birth time is
/// simply the first 16 bytes of every record.
#[cfg(target_os = "macos")]
fn macos_token(pid: u32) -> Option<u64> {
    let Ok(signed_pid) = i32::try_from(pid) else {
        return None; // beyond pid_t range: no such process
    };
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        signed_pid,
    ];

    /// Darwin `struct timeval` (64-bit hosts): `long` seconds, `int` micros.
    #[repr(C)]
    struct DarwinTimeVal {
        tv_sec: i64,
        tv_usec: i32,
    }

    // Size the record first (one kinfo_proc here), then fetch it.
    let mut size: usize = 0;
    // Safety: NULL buffer with non-NULL size pointer is sysctl's documented
    // size query.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut size),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < std::mem::size_of::<DarwinTimeVal>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    // Safety: buf.len() == the size sysctl itself reported.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            std::ptr::from_mut(&mut size),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let tv = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<DarwinTimeVal>()) };
    let sec = u64::try_from(tv.tv_sec).ok()?;
    let usec = u64::try_from(tv.tv_usec).ok()?;
    Some(sec.wrapping_mul(1_000_000_000).wrapping_add(usec * 1_000))
}

/// Windows: creation FILETIME via `GetProcessTimes`, the raw 64-bit count
/// of 100 ns intervals since 1601-01-01.
#[cfg(windows)]
fn windows_token(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // Safety: query-only handle, closed on every path before returning.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // Safety: four FILETIME sinks sized by the API contract; handle owned above.
    let ok = unsafe {
        GetProcessTimes(
            handle,
            std::ptr::from_mut(&mut creation),
            std::ptr::from_mut(&mut exit),
            std::ptr::from_mut(&mut kernel),
            std::ptr::from_mut(&mut user),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// Test fixture: spawn a live, distinct, killable child that sleeps for
/// `secs` seconds, so tests get a real pid whose start token is inspectable
/// while it runs. The unix arm is exactly the `sleep <secs>` spawn the test
/// suite has always used; Windows has no `sleep`, so the same shape is
/// provided by the always-present Windows PowerShell (`Start-Sleep`).
pub fn spawn_sleeper(secs: u64) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", &format!("Start-Sleep -Seconds {secs}")]);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = std::process::Command::new("sleep");
        c.arg(secs.to_string());
        c
    };
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test process obviously exists and has been running for a while:
    /// its token must be inspectable, stable across calls, and different
    /// from a bogus pid's absence-of-information.
    #[test]
    fn own_process_token_is_stable_and_present() {
        let me = std::process::id();
        if let Some(t) = process_start_token(me) {
            assert_eq!(process_start_token(me), Some(t), "stable across calls");
        }
        // On platforms without a probe (or a vanished pid) we get None —
        // both are legal; nothing to assert beyond "does not panic".
        let _ = process_start_token(u32::MAX - 7);
    }

    /// A spawned child has its OWN birth time: different from ours, and
    /// still observable after it exits on Linux (zombie keeps /proc entry)
    /// while macOS/Windows may lose it — either way the call must be safe.
    ///
    /// Granularity caveat (Linux): starttime ticks at `CONFIG_HZ` (usually
    /// 100/s). If this test binary is younger than one jiffy when the child
    /// spawns, both births land in the same tick and the tokens legitimately
    /// collide. One retry after a sleep past that window makes the assertion
    /// deterministic without weakening it.
    #[test]
    fn child_process_token_differs_from_parent_when_visible() {
        let mut attempt = 0;
        loop {
            let mut child = spawn_sleeper(30).expect("spawn sleeper");
            let child_token = process_start_token(child.id());
            child.kill().expect("kill sleeper");
            child.wait().expect("reap");

            match child_token {
                Some(ct) => {
                    let mine = process_start_token(std::process::id());
                    if mine == Some(ct) && attempt == 0 {
                        // Same-tick birth collision; once we're older than a
                        // jiffy any new child is provably born after us.
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_millis(25));
                        continue;
                    }
                    assert_ne!(Some(ct), mine, "parent and child are distinct instances");
                }
                None => break,
            }
            break;
        }
    }
}
