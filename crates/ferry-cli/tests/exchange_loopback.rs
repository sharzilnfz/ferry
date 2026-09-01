use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Device {
    home: tempfile::TempDir,
    _tree_dir: tempfile::TempDir,
    tree: PathBuf,
}

impl Device {
    fn new(_tag: &str) -> Device {
        let home = tempfile::tempdir().expect("home dir");
        let tree_dir = tempfile::tempdir().expect("tree dir");
        let tree = tree_dir.path().to_path_buf();
        Device {
            home,
            _tree_dir: tree_dir,
            tree,
        }
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

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_file(path: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut sleep_ms = 10u64;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
    }
    false
}

#[test]
fn two_devices_pair_and_converge_over_localhost() {
    let a = Device::new("a");
    let b = Device::new("b");

    let out = a.command(&["init"]).output().expect("run init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::write(a.tree.join("hello.txt"), b"hello from device A\n").unwrap();
    std::fs::create_dir_all(a.tree.join("src")).unwrap();
    std::fs::write(a.tree.join("src/main.py"), b"print('hi')\n").unwrap();

    std::fs::write(
        a.tree.join(".env"),
        "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();

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

    let log_dir = std::env::temp_dir().join(format!("ferry-test-logs-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&log_dir);
    let mut daemon_a_cmd =
        a.command(&["daemon", "--listen", "127.0.0.1:0", "--interval-secs", "1"]);
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

    let hello_a_path = a.tree.join("hello.txt");
    let hello_b = b.tree.join("hello.txt");
    let src_b = b.tree.join("src/main.py");
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut sleep_ms = 10u64;
    loop {
        let got_hello = std::fs::read(&hello_b).ok() == Some(b"hello from device A\n".to_vec());
        let got_src = src_b.is_file();
        if got_hello && got_src {
            break;
        }
        assert!(Instant::now() < deadline, "B never converged: {addr}");
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 200 {
            sleep_ms = (sleep_ms * 2).min(200);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut sleep_ms = 10u64;
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
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 400 {
            sleep_ms = (sleep_ms * 2).min(400);
        }
    }

    std::fs::write(a.tree.join("late.txt"), b"written after start\n").unwrap();
    let late_b = b.tree.join("late.txt");
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut sleep_ms = 10u64;
    loop {
        if std::fs::read(&late_b).ok().as_deref() == Some(b"written after start\n".as_slice()) {
            break;
        }
        assert!(Instant::now() < deadline, "live change never reached B");
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 200 {
            sleep_ms = (sleep_ms * 2).min(200);
        }
    }

    std::fs::remove_file(a.tree.join("hello.txt")).unwrap();
    let mut timeline: Vec<String> = Vec::new();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(10);
    let mut sleep_ms = 10u64;
    loop {
        let a_has = hello_a_path.exists();
        let b_has = hello_b.exists();
        timeline.push(format!(
            "+{:>5}ms A={} B={}",
            start.elapsed().as_millis(),
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
                "deletion never reached B within 10s (has_local_wins fix should land in <5s)\nTIMELINE:\n{}\n=== A TREE: {}\n=== B TREE: {}\n=== A LOG:\n{}\n=== B LOG:\n{}",
                timeline.join("\n"),
                list(&a.tree),
                list(&b.tree),
                std::fs::read_to_string(log_dir.join("a.log")).unwrap_or_default(),
                std::fs::read_to_string(log_dir.join("b.log")).unwrap_or_default(),
            );
        }
        std::thread::sleep(Duration::from_millis(sleep_ms));
        if sleep_ms < 100 {
            sleep_ms = (sleep_ms * 2).min(100);
        }
    }

    assert!(
        !b.tree.join(".env").exists(),
        ".env must not sync under default rules"
    );

    for d in [&a, &b] {
        let out = d
            .command(&["conflicts", "list", "--json"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
        assert_eq!(doc["entries"].as_array().unwrap().len(), 0, "{doc}");
    }

    let out = b.command(&["status", "--json"]).output().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let peers = doc["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1, "{doc}");
    assert!(peers[0]["last_agreed_manifest_id"].is_string(), "{doc}");

    drop(daemon_a);
    drop(daemon_b);
}

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

fn read_listening<R: std::io::Read + Send + 'static>(r: R, secs: u64) -> Option<String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(r);
        for line in reader.lines() {
            match line {
                Ok(l) if l.starts_with("LISTENING ") => {
                    let addr = l["LISTENING ".len()..].trim().to_string();
                    let _ = tx.send(addr);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}
