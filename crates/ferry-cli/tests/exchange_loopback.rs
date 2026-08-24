//! End-to-end loopback: two simulated devices on one machine pair via the
//! file ritual and converge over localhost TCP using the real `ferry`
//! binary. This is the scripted core of scripts/quickstart-e2e.sh and of
//! the five-minute acceptance.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Device {
    home: tempfile::TempDir,
    tree: PathBuf,
}

impl Device {
    fn new(tag: &str) -> Device {
        let home = tempfile::tempdir().expect("home dir");
        let tree =
            std::env::temp_dir().join(format!("ferry-cli-e2e-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        std::fs::create_dir_all(&tree).unwrap();
        Device { home, tree }
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

fn ferry_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_ferry")
        .map(PathBuf::from)
        .expect("bin built by cargo")
}

/// Kill a daemon when the test ends, however it ends.
struct Daemon(Child);

impl Drop for Daemon {
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
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn two_devices_pair_and_converge_over_localhost() {
    let a = Device::new("a");
    let b = Device::new("b");

    // --- device A: init + content -----------------------------------------
    let out = a.command(&["init"]).output().expect("run init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::write(a.tree.join("hello.txt"), b"hello from device A\n").unwrap();
    std::fs::create_dir_all(a.tree.join("src")).unwrap();
    std::fs::write(a.tree.join("src/main.py"), b"print('hi')\n").unwrap();
    // .env stays OUT by default; assert below that B never receives it.
    std::fs::write(
        a.tree.join(".env"),
        "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();

    // --- pairing ritual via payload files ----------------------------------
    let mut pair_a = a
        .command(&["pair", "--timeout-secs", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let offer = a.tree.join(".ferry/pair-offer.ferry-pair");
    assert!(wait_for_file(&offer, 30), "device A never wrote its offer");

    let out = b
        .command(&[
            "pair",
            "--accept",
            offer.to_str().unwrap(),
            "--timeout-secs",
            "60",
        ])
        .output()
        .expect("pair accept");
    assert!(
        out.status.success(),
        "accept failed: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let status_a = wait_for(&mut pair_a, 60);
    assert!(status_a, "device A's `ferry pair` never completed");
    assert!(b.tree.join(".ferry/config").is_file(), "B adopted a store");

    // --- daemons: A listens, B drives ---------------------------------------
    let log_dir = std::env::temp_dir().join(format!("ferry-test-logs-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&log_dir);
    let mut daemon_a_cmd = a.command(&["daemon", "--listen", "127.0.0.1:0"]);
    daemon_a_cmd
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(log_dir.join("a.log")).unwrap());
    let mut daemon_a = Daemon(daemon_a_cmd.spawn().expect("daemon A"));
    let addr =
        read_listening(daemon_a.0.stdout.take().unwrap(), 15).expect("daemon A printed LISTENING");

    let daemon_b_stderr = std::fs::File::create(log_dir.join("b.log")).unwrap();
    let mut daemon_b = Daemon(
        b.command(&["daemon", "--peer-url", &addr, "--interval-secs", "1"])
            .stdout(Stdio::null())
            .stderr(daemon_b_stderr)
            .spawn()
            .expect("daemon B"),
    );
    let _ = &mut daemon_b;

    // --- convergence ---------------------------------------------------------
    let hello_a_path = a.tree.join("hello.txt");
    let hello_b = b.tree.join("hello.txt");
    let src_b = b.tree.join("src/main.py");
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let got_hello = std::fs::read(&hello_b).ok() == Some(b"hello from device A\n".to_vec());
        let got_src = src_b.is_file();
        if got_hello && got_src {
            break;
        }
        assert!(Instant::now() < deadline, "B never converged: {addr}");
        std::thread::sleep(Duration::from_millis(250));
    }

    // --- agreement settles, THEN live change propagates A -> B --------------
    // Deletions only carry meaning against a recorded agreement: before the
    // first settle, "present on one side" wins by design (safe default). So
    // wait for the agreement pointer before mutating anything.
    let deadline = Instant::now() + Duration::from_mins(1);
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
        assert!(Instant::now() < deadline, "agreement never settled: {doc}");
        std::thread::sleep(Duration::from_millis(400));
    }

    std::fs::write(a.tree.join("late.txt"), b"written after start\n").unwrap();
    let late_b = b.tree.join("late.txt");
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        if std::fs::read(&late_b).ok().as_deref() == Some(b"written after start\n".as_slice()) {
            break;
        }
        assert!(Instant::now() < deadline, "live change never reached B");
        std::thread::sleep(Duration::from_millis(250));
    }

    // --- deletion propagates too --------------------------------------------
    std::fs::remove_file(a.tree.join("hello.txt")).unwrap();
    let mut timeline: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let a_has = hello_a_path.exists();
        let b_has = hello_b.exists();
        timeline.push(format!(
            "+{:>5}ms A={} B={}",
            deadline.elapsed().as_millis() + 60_000,
            a_has,
            b_has
        ));
        if !b_has {
            break;
        }
        if Instant::now() >= deadline {
            let list = |root: &std::path::Path| {
                std::fs::read_dir(root).map_or_else(
                    |e| format!("err {e}"),
                    |rd| {
                        rd.flatten()
                            .map(|e| {
                                format!(
                                    "{} {}",
                                    e.file_name().to_string_lossy(),
                                    e.metadata().map_or(0, |m| m.len())
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                )
            };
            panic!(
                "deletion never reached B\nTIMELINE:\n{}\n=== A TREE: {}\n=== B TREE: {}\n=== A LOG:\n{}\n=== B LOG:\n{}",
                timeline.join("\n"),
                list(&a.tree),
                list(&b.tree),
                std::fs::read_to_string(log_dir.join("a.log")).unwrap_or_default(),
                std::fs::read_to_string(log_dir.join("b.log")).unwrap_or_default(),
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // --- secrets hygiene: default rules keep .env local ---------------------
    assert!(
        !b.tree.join(".env").exists(),
        ".env must not sync under default rules"
    );

    // --- conflicts report empty on both sides --------------------------------
    for d in [&a, &b] {
        let out = d
            .command(&["conflicts", "list", "--json"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
        assert_eq!(doc["entries"].as_array().unwrap().len(), 0, "{doc}");
    }

    // --- agreement state visible in status ----------------------------------
    let out = b.command(&["status", "--json"]).output().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let peers = doc["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1, "{doc}");
    assert!(peers[0]["last_agreed_manifest_id"].is_string(), "{doc}");

    drop(daemon_a);
    drop(daemon_b);
    let _ = std::fs::remove_dir_all(&a.tree);
    let _ = std::fs::remove_dir_all(&b.tree);
}

/// Wait for a child to exit successfully within `secs`.
fn wait_for(child: &mut Child, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait().unwrap() {
            Some(st) => return st.success(),
            None if Instant::now() > deadline => return false,
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Read stdout until the `LISTENING <addr>` line appears.
fn read_listening<R: std::io::Read>(r: R, secs: u64) -> Option<String> {
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
