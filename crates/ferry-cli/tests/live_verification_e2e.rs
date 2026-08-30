//! Live process end-to-end verification tests covering all acceptance criteria
//! from .scratch/live-verification-fixes/issues/09-e2e-live-process-and-browser-verification.md.

mod common;

use common::{Env, RunningDaemon};
use ferry_cli::commands;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn ferry_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_ferry").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ferry"),
        PathBuf::from,
    )
}

struct TestDevice {
    home: tempfile::TempDir,
    tree: PathBuf,
}

impl TestDevice {
    fn new(tag: &str) -> Self {
        let home = tempfile::tempdir().expect("home dir");
        let tree =
            std::env::temp_dir().join(format!("ferry-e2e-test-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir_all(&tree).unwrap();
        Self { home, tree }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(ferry_bin());
        c.args(args)
            .env("FERRY_HOME", self.home.path())
            .current_dir(&self.tree)
            .env("RUST_LOG", "");
        c
    }
}

struct ProcDaemon(Child);

impl Drop for ProcDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_file(path: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_child(child: &mut Child, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait().unwrap() {
            Some(st) => return st.success(),
            None if Instant::now() > deadline => return false,
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn read_listening<R: std::io::Read>(r: R, secs: u64) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let reader = BufReader::new(r);
    let deadline = Instant::now() + Duration::from_secs(secs);
    for line in reader.lines() {
        if Instant::now() > deadline {
            return None;
        }
        if let Ok(line) = line {
            if let Some(addr) = line.strip_prefix("LISTENING ") {
                return Some(addr.trim().to_string());
            }
        }
    }
    None
}

#[test]
fn test_unpinned_concurrent_edit_quarantines_and_logs_conflict() {
    let a = TestDevice::new("conflict-a");
    let b = TestDevice::new("conflict-b");

    // 1. Init on device A + create initial file
    let out = a.command(&["init"]).output().expect("init a");
    assert!(out.status.success());
    fs::write(a.tree.join("shared.txt"), b"initial base line\n").unwrap();

    // 2. Pairing
    let mut pair_a = a
        .command(&["pair", "--timeout-secs", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let offer = a.tree.join(".ferry/pair-offer.ferry-pair");
    assert!(
        wait_for_file(&offer, 15),
        "device A offer file never appeared"
    );

    let out = b
        .command(&[
            "pair",
            "--accept",
            offer.to_str().unwrap(),
            "--timeout-secs",
            "30",
        ])
        .output()
        .expect("pair accept");
    assert!(
        out.status.success(),
        "accept failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(wait_for_child(&mut pair_a, 30), "pair a failed to finish");

    // 3. Start daemons: A listens, B dials
    let mut daemon_a_cmd = a.command(&["daemon", "--listen", "127.0.0.1:0"]);
    daemon_a_cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut daemon_a = ProcDaemon(daemon_a_cmd.spawn().expect("daemon A"));
    let addr = read_listening(daemon_a.0.stdout.take().unwrap(), 15).expect("daemon A listen addr");

    let mut daemon_b = ProcDaemon(
        b.command(&["daemon", "--peer-url", &addr, "--interval-secs", "1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon B"),
    );
    let _ = &mut daemon_b;

    // 4. Wait for initial agreement
    let shared_b = b.tree.join("shared.txt");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !shared_b.exists() {
        assert!(Instant::now() < deadline, "shared.txt never reached B");
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(
        fs::read_to_string(&shared_b).unwrap(),
        "initial base line\n"
    );

    // Wait for agreement status to settle
    let agree_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = b.command(&["status", "--json"]).output().unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let agreed = doc["peers"]
            .as_array()
            .and_then(|p| p.first())
            .and_then(|p| p["last_agreed_manifest_id"].as_str())
            .is_some();
        if agreed {
            break;
        }
        assert!(
            Instant::now() < agree_deadline,
            "agreement never settled: {doc}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // 5. Concurrently edit shared.txt on both sides while UNPINNED
    fs::write(a.tree.join("shared.txt"), b"mod from device A (newer)\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    fs::write(
        b.tree.join("shared.txt"),
        b"mod from device B (concurrent)\n",
    )
    .unwrap();

    // Give daemons time to exchange, reconcile, and quarantine the loser
    let reconcile_deadline = Instant::now() + Duration::from_secs(20);
    let mut quarantined = false;
    while Instant::now() < reconcile_deadline {
        let files_a: Vec<String> = fs::read_dir(&a.tree)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let files_b: Vec<String> = fs::read_dir(&b.tree)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        if files_a.iter().any(|f| f.contains(".ferry-conflict."))
            || files_b.iter().any(|f| f.contains(".ferry-conflict."))
        {
            quarantined = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // Also check conflicts list via CLI
    let conflicts_out_a = a
        .command(&["conflicts", "list", "--json"])
        .output()
        .unwrap();
    let doc_a: serde_json::Value = serde_json::from_slice(&conflicts_out_a.stdout).unwrap();
    let entries_a = doc_a["entries"].as_array().map_or(0, Vec::len);

    let conflicts_out_b = b
        .command(&["conflicts", "list", "--json"])
        .output()
        .unwrap();
    let doc_b: serde_json::Value = serde_json::from_slice(&conflicts_out_b.stdout).unwrap();
    let entries_b = doc_b["entries"].as_array().map_or(0, Vec::len);

    assert!(
        quarantined || entries_a > 0 || entries_b > 0,
        "Concurrent unpinned edit must produce quarantine file or conflict log entry: A={entries_a} B={entries_b}"
    );

    drop(daemon_a);
    drop(daemon_b);
    let _ = fs::remove_dir_all(&a.tree);
    let _ = fs::remove_dir_all(&b.tree);
}

#[test]
fn test_cli_pin_hours_persists_across_cli_invocations() {
    let env = Env::new("cli_pin_hours_e2e");
    let proj = env.work().join("pin_proj");
    fs::create_dir_all(&proj).unwrap();

    commands::init::run(&proj).expect("init proj");
    let daemon = RunningDaemon::start(&proj);

    // Run pin start with --hours 8
    let out = commands::pin::start(&proj, &["src/**".to_string()], 8).expect("pin start");
    assert_eq!(out.json["command"], "pin");
    assert_eq!(out.json["action"], "start");

    // Verify status immediately after CLI exits
    let status_out = commands::pin::status(&proj).expect("pin status");
    assert_eq!(status_out.json["state"], "active");
    assert_eq!(status_out.json["holding"], true);
    assert_eq!(status_out.json["paths"], serde_json::json!(["src/**"]));

    // Verify record in .ferry/pin-state.json has expires_sec set
    let state_file = proj.join(".ferry/pin-state.json");
    let content = fs::read_to_string(&state_file).expect("read pin state");
    let json_val: serde_json::Value = serde_json::from_str(&content).expect("parse pin state");
    assert!(
        json_val.get("expires_sec").is_some(),
        "expires_sec must be recorded in pin state"
    );

    drop(daemon);
}

#[test]
fn test_cli_ignore_external_folder_targeting() {
    let env = Env::new("cli_ignore_external_folder");
    let proj = env.work().join("ext_proj");
    fs::create_dir_all(&proj).unwrap();

    commands::init::run(&proj).expect("init ext proj");

    // Run ignore list specifying the external folder path
    let list_out = commands::ignore_cmd::run(&proj, None, None, true).expect("ignore list");
    assert_eq!(list_out.json["command"], "ignore");
    assert_eq!(list_out.json["folder"], proj.display().to_string());
    assert!(list_out.json.get("layers").is_some());

    // Apply a pattern to external folder
    let add_out =
        commands::ignore_cmd::run(&proj, Some("*.log"), None, false).expect("add pattern");
    assert_eq!(add_out.json["action"], "added-line");

    // Verify ferry.ignore in the external directory contains the rule
    let ignore_file = proj.join("ferry.ignore");
    assert!(ignore_file.exists());
    let ignore_text = fs::read_to_string(&ignore_file).unwrap();
    assert!(ignore_text.contains("*.log"));

    // Apply a preset to external folder
    let preset_out =
        commands::ignore_cmd::run(&proj, None, Some("claude"), false).expect("apply preset");
    assert_eq!(preset_out.json["preset"], "claude");

    // Verify listing layers includes presets
    let list_after = commands::ignore_cmd::run(&proj, None, None, true).expect("ignore list after");
    assert_eq!(
        list_after.json["applied_presets"],
        serde_json::json!(["claude"])
    );
}

#[test]
#[allow(deprecated)]
fn test_ui_events_and_token_auth_flow() {
    use ferry_daemon::ui::server::{generate_token, DashboardServer};
    use ferry_ipc::backend::connect_auto;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let env = Env::new("ui_events_and_auth");
    let proj = env.work().join("ui_proj");
    fs::create_dir_all(&proj).unwrap();
    commands::init::run(&proj).expect("init ui proj");

    let token = generate_token();
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&proj);
    let backend = Arc::new(connect_auto(socket_path, proj.clone()));
    let server = DashboardServer::new(backend).with_token(token.clone());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            server.serve(listener).await.unwrap();
        });

        // 1. Without token, GET /api/events returns 403
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!("GET /api/events HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("403 Forbidden"),
            "Unauthenticated SSE should return 403, got {resp_str}"
        );

        // 2. With token query param, GET /api/events returns 200 text/event-stream
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /api/events?token={token} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let resp_str = String::from_utf8_lossy(&buf[..n]);
        assert!(
            resp_str.contains("200 OK"),
            "Authenticated SSE should return 200 OK, got {resp_str}"
        );
        assert!(
            resp_str.contains("text/event-stream"),
            "Content-type should be text/event-stream"
        );

        // 3. Root index.html serves with 200 without token (public SPA entry)
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"));
        assert!(resp_str.contains("ferry"));
        assert!(resp_str.contains("Protected by session token"));

        server_task.abort();
    });
}
