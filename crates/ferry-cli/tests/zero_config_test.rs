use std::time::{Duration, Instant};

#[test]
fn test_zero_config_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let home_a = tmp.path().join("home-a");
    let home_b = tmp.path().join("home-b");
    let tree_a = tmp.path().join("tree-a");
    let tree_b = tmp.path().join("tree-b");
    std::fs::create_dir_all(&home_a).unwrap();
    std::fs::create_dir_all(&home_b).unwrap();
    std::fs::create_dir_all(&tree_a).unwrap();
    std::fs::create_dir_all(&tree_b).unwrap();

    std::fs::write(tree_a.join("hello.txt"), "hello zero config\n").unwrap();

    let exe = env!("CARGO_BIN_EXE_ferry");

    // 1. ferry share on tree A
    let share_out = std::process::Command::new(exe)
        .arg("share")
        .arg("--json")
        .current_dir(&tree_a)
        .env("FERRY_HOME", &home_a)
        .output()
        .unwrap();

    assert!(share_out.status.success(), "share failed: {:?}", String::from_utf8_lossy(&share_out.stderr));
    let share_json: serde_json::Value = serde_json::from_slice(&share_out.stdout).unwrap();
    let code = share_json["code"].as_str().unwrap();
    eprintln!("Got pairing code: {code}");

    // 2. ferry join on tree B
    let join_out = std::process::Command::new(exe)
        .arg("join")
        .arg(code)
        .arg(&tree_b)
        .arg("--json")
        .env("FERRY_HOME", &home_b)
        .output()
        .unwrap();

    assert!(join_out.status.success(), "join failed: {:?}", String::from_utf8_lossy(&join_out.stderr));
    eprintln!("Join output: {:?}", String::from_utf8_lossy(&join_out.stdout));

    // 3. Wait for hello.txt in tree B
    let deadline = Instant::now() + Duration::from_secs(10);
    let target_file = tree_b.join("hello.txt");
    while Instant::now() < deadline {
        if target_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !target_file.exists() {
        let log_a = std::fs::read_to_string(home_a.join("daemon.log")).unwrap_or_default();
        eprintln!("=== LOG A (non-state lines) ===");
        for line in log_a.lines().filter(|l| !l.contains("STATE root=")) {
            eprintln!("{line}");
        }
        let log_b = std::fs::read_to_string(home_b.join("daemon.log")).unwrap_or_default();
        eprintln!("=== LOG B (non-state lines) ===");
        for line in log_b.lines().filter(|l| !l.contains("STATE root=")) {
            eprintln!("{line}");
        }
        panic!("hello.txt never arrived in tree B");
    }
}
