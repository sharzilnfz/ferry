




use ferry_cli::commands::daemon::{status_in, stop_in};

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn stamp(pid: u32, token: Option<u64>) -> String {
    match token {
        Some(token) => format!("{pid} {token}\n"),
        None => format!("{pid}\n"),
    }
}

#[test]
fn stop_and_status_on_empty_home_report_not_running() {
    let home = temp_home();

    let stop = stop_in(home.path()).unwrap();
    assert_eq!(stop.json["command"], "daemon");
    assert_eq!(stop.json["action"], "stop");
    assert_eq!(stop.json["status"], "not_running");
    assert_eq!(stop.exit_code, 0);

    let status = status_in(home.path()).unwrap();
    assert_eq!(status.json["action"], "status");
    assert_eq!(status.json["status"], "stopped");
    assert_eq!(status.exit_code, 0);
}

#[test]
fn status_reports_running_for_live_pid_with_matching_start_token() {
    let home = temp_home();
    let me = std::process::id();
    let token = ferry_platform::process_start_token(me);
    std::fs::write(home.path().join("daemon.pid"), stamp(me, token)).unwrap();

    let status = status_in(home.path()).unwrap();
    assert_eq!(status.json["status"], "running");
    assert_eq!(status.json["pid"].as_u64(), Some(u64::from(me)));
}

#[test]
fn status_reports_stopped_when_pid_file_points_at_a_dead_process() {
    let home = temp_home();
    let mut child = ferry_platform::spawn_sleeper(30).unwrap();
    let token = ferry_platform::process_start_token(child.id());
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::write(home.path().join("daemon.pid"), stamp(child.id(), token)).unwrap();

    let status = status_in(home.path()).unwrap();
    assert_eq!(status.json["status"], "stopped");
}

#[test]
fn status_refuses_a_reused_pid_whose_token_belongs_to_another_instance() {
    let home = temp_home();
    
    
    let mut child = ferry_platform::spawn_sleeper(30).unwrap();
    let token = ferry_platform::process_start_token(child.id()).map(|t| t.wrapping_add(1));
    std::fs::write(home.path().join("daemon.pid"), stamp(child.id(), token)).unwrap();

    let status = status_in(home.path()).unwrap();
    assert_eq!(status.json["status"], "stopped");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn stop_kills_the_recorded_daemon_and_clears_pid_and_socket_files() {
    let home = temp_home();
    let child = ferry_platform::spawn_sleeper(30).unwrap();
    let token = ferry_platform::process_start_token(child.id());
    std::fs::write(home.path().join("daemon.pid"), stamp(child.id(), token)).unwrap();
    std::fs::write(home.path().join("daemon.sock"), b"stale").unwrap();

    let stop = stop_in(home.path()).unwrap();

    assert_eq!(stop.json["status"], "stopped");
    assert_eq!(stop.json["pid"].as_u64(), Some(u64::from(child.id())));
    assert_eq!(stop.exit_code, 0);
    assert!(
        !home.path().join("daemon.pid").exists(),
        "pid file unlinked only after the OS confirmed exit"
    );
    assert!(
        !home.path().join("daemon.sock").exists(),
        "socket unlinked after exit"
    );
    assert!(
        ferry_platform::process_start_token(child.id()).is_none(),
        "the OS confirms the daemon process is gone"
    );
    drop(child);
}
