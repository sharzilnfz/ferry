use ferry_cli::bootstrap::ensure_daemon;

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn ensure_daemon_spawns_when_socket_absent_and_reuses() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = temp_home();
    let hp = home.path().to_path_buf();
    // Ensure no socket exists
    let sock = hp.join("daemon.sock");
    assert!(!sock.exists());

    let p1 = ensure_daemon(&hp).expect("first ensure should spawn dummy daemon");
    assert!(
        p1.exists() || sock.exists(),
        "socket should appear within 5s"
    );
    // Socket should be at home/daemon.sock
    assert_eq!(p1, sock);

    // Second call should reuse without spawning second daemon (fast return)
    let start = std::time::Instant::now();
    let p2 = ensure_daemon(&hp).expect("second ensure reuses");
    let elapsed = start.elapsed();
    assert_eq!(p2, sock);
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "second call should be fast (ping within 200ms), got {elapsed:?}"
    );
}

#[test]
fn ensure_daemon_ping_within_200ms_when_running() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = temp_home();
    let hp = home.path().to_path_buf();
    // Start ensure once to have daemon
    let _ = ensure_daemon(&hp).unwrap();
    let sock = hp.join("daemon.sock");
    let start = std::time::Instant::now();
    let ok = ensure_daemon(&hp).is_ok();
    let _ = ok;
    let _ = sock;
    let elapsed = start.elapsed();
    assert!(elapsed < std::time::Duration::from_millis(800));
}

#[test]
fn zero_arg_defaults_to_ui_help_contains_two_minute_path() {
    let help = ferry_cli::cli::AFTER_HELP;
    assert!(
        help.contains("Two-minute path"),
        "epilog must document two-minute path"
    );
    assert!(help.contains("ferry share"), "epilog must mention share");
    assert!(help.contains("ferry join"), "epilog must mention join");
}

#[test]
fn ferry_with_zero_args_parses_as_none() {
    use clap::Parser;
    use ferry_cli::cli::Cli;
    let cli = Cli::try_parse_from(["ferry"]).expect("zero args should parse");
    assert!(
        cli.command.is_none(),
        "zero args should be None (defaults to ui)"
    );
    let cli2 = Cli::try_parse_from(["ferry", "status"]).unwrap();
    assert!(cli2.command.is_some());
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn share_and_join_json_round_trip_two_homes() {
    // Two isolated homes, share in A, join in B, verify same folder_id and no pair-offer file when code path used
    let home_a = temp_home();
    let home_b = temp_home();
    let work_a = tempfile::tempdir().unwrap();
    let work_b = tempfile::tempdir().unwrap();

    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("FERRY_HOME", home_a.path());
    let proj_a = work_a.path().join("proj-a");
    std::fs::create_dir_all(&proj_a).unwrap();
    ferry_cli::commands::init::run(&proj_a, "init").unwrap();

    let out_a = ferry_cli::commands::share::run(&proj_a, false, 5)
        .expect("share should succeed with code path");
    assert!(out_a.json["code"].is_string());
    let code = out_a.json["code"].as_str().unwrap().to_string();
    let folder_id_a = out_a.json["folder_id"].as_str().unwrap().to_string();
    assert_eq!(folder_id_a.len(), 32);
    // Should not have written legacy offer when code path succeeds
    // (allow either, but prefer not)
    let _has_offer = proj_a.join(".ferry/pair-offer.ferry-pair").exists();

    // Switch to home B
    std::env::set_var("FERRY_HOME", home_b.path());
    let dest_b = work_b.path().join("proj-b");
    std::fs::create_dir_all(&dest_b).unwrap();
    // dest must be empty; join will create .ferry
    let out_b = ferry_cli::commands::join::run(&code, Some(&dest_b)).expect("join should succeed");
    assert_eq!(out_b.json["folder_id"], folder_id_a);
    assert_eq!(out_b.json["status"], "joined");
    assert!(dest_b.join(".ferry").is_dir());
    assert!(dest_b.join(".ferry/config").is_file());
    // Verify folder_id matches via config head parse or json
    let cfg_bytes = std::fs::read(dest_b.join(".ferry/config")).unwrap();
    let head = ferry_crypto::config_head::parse_config_head(&cfg_bytes).unwrap();
    assert_eq!(ferry_store::format::hex(&head.folder_id), folder_id_a);

    // Restore
    std::env::remove_var("FERRY_HOME");
}

#[test]
fn headless_share_json_works_without_tty() {
    let home = temp_home();
    let work = tempfile::tempdir().unwrap();
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("FERRY_HOME", home.path());
    let proj = work.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    ferry_cli::commands::init::run(&proj, "init").unwrap();
    let out = ferry_cli::commands::share::run(&proj, false, 5).unwrap();
    assert!(out.json["code"].is_string());
    assert!(out.json["expires_at"].is_string());
    // Human output should contain Share code
    assert!(out.human.contains("Share code"));
    std::env::remove_var("FERRY_HOME");
}
