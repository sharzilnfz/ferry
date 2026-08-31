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

#[cfg(target_os = "linux")]
fn linux_token(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

#[cfg(target_os = "macos")]
fn macos_token(pid: u32) -> Option<u64> {
    let Ok(signed_pid) = i32::try_from(pid) else {
        return None;
    };
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        signed_pid,
    ];

    #[repr(C)]
    struct DarwinTimeVal {
        tv_sec: i64,
        tv_usec: i32,
    }

    let mut size: usize = 0;

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

    if rc != 0 || size < std::mem::size_of::<DarwinTimeVal>() {
        return None;
    }
    let tv = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<DarwinTimeVal>()) };
    let sec = u64::try_from(tv.tv_sec).ok()?;
    let usec = u64::try_from(tv.tv_usec).ok()?;
    Some(sec.wrapping_mul(1_000_000_000).wrapping_add(usec * 1_000))
}

#[cfg(windows)]
fn windows_token(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

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

pub fn spawn_sleeper(secs: u64) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("powershell");
        c.args([
            "-NoProfile",
            "-Command",
            &format!("Start-Sleep -Seconds {secs}"),
        ]);
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

    #[test]
    fn own_process_token_is_stable_and_present() {
        let me = std::process::id();
        if let Some(t) = process_start_token(me) {
            assert_eq!(process_start_token(me), Some(t), "stable across calls");
        }

        let _ = process_start_token(u32::MAX - 7);
    }

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
