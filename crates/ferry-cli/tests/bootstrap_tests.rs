use ferry_cli::bootstrap::ensure_daemon;

fn temp_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn ensure_daemon_fails_with_daemon_start_failed_when_binary_missing() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = temp_home();
    let hp = home.path().to_path_buf();
    let sock = hp.join("daemon.sock");
    assert!(!sock.exists());

    let prev = std::env::var("FERRY_BIN").ok();
    std::env::set_var("FERRY_BIN", "/nonexistent/ferry-missing-binary-xyz");
    let res = ensure_daemon(&hp);

    match prev {
        Some(v) => std::env::set_var("FERRY_BIN", v),
        None => std::env::remove_var("FERRY_BIN"),
    }
    let err = res.expect_err("ensure_daemon should fail when binary missing");
    assert_eq!(err.code, "daemon-start-failed");
    assert!(
        err.hint.contains("check $FERRY_HOME permissions"),
        "hint must contain permissions guidance, got {}",
        err.hint
    );
    assert!(
        err.message.contains(&sock.display().to_string())
            || err.message.contains("daemon failed to start"),
        "message should mention socket or failure, got {}",
        err.message
    );
}

#[test]
fn ensure_daemon_reuses_running_server_via_ping() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = temp_home();
    let hp = home.path().to_path_buf();
    let sock = hp.join("daemon.sock");
    assert!(!sock.exists());

    let sock_clone = sock.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _ = std::fs::remove_file(&sock_clone);
            if let Some(parent) = sock_clone.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let Ok(server) = ferry_ipc::IpcServer::bind(&sock_clone) else {
                return;
            };
            loop {
                let conn_res = server.accept().await;
                if let Ok(conn) = conn_res {
                    tokio::spawn(async move {
                        let mut c = conn;
                        let snap = ferry_ipc::EngineSnapshot::new("", "", "", "idle");
                        let _ = c
                            .send_message(&ferry_ipc::DaemonMessage::Snapshot(snap))
                            .await;
                        loop {
                            match c.recv_command().await {
                                Ok(Some(ferry_ipc::ClientCommand::Ping)) => {
                                    let _ = c.send_message(&ferry_ipc::DaemonMessage::Pong).await;
                                }
                                Ok(Some(_)) => {
                                    let _ = c
                                        .send_message(&ferry_ipc::DaemonMessage::Ack {
                                            command: "ok".into(),
                                            message: None,
                                        })
                                        .await;
                                }
                                Ok(None) => break,
                                Err(_) => break,
                            }
                        }
                    });
                }
            }
        });
    });

    let mut waited = std::time::Duration::from_millis(0);
    while waited < std::time::Duration::from_millis(2000) {
        if sock.exists() {
            let probe = ensure_daemon(&hp);
            if probe.is_ok() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += std::time::Duration::from_millis(50);
    }
    assert!(sock.exists(), "fake server should have created socket");

    let start = std::time::Instant::now();
    let p2 = ensure_daemon(&hp).expect("second ensure should reuse running server");
    let elapsed = start.elapsed();
    assert_eq!(p2, sock);
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "reuse via ping should be fast, got {elapsed:?}"
    );

    let start2 = std::time::Instant::now();
    let ok = ensure_daemon(&hp).is_ok();
    assert!(ok);
    assert!(start2.elapsed() < std::time::Duration::from_millis(800));
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
    let home_a = temp_home();
    let home_b = temp_home();
    let work_a = tempfile::tempdir().unwrap();
    let work_b = tempfile::tempdir().unwrap();

    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("FERRY_HOME", home_a.path());
    let proj_a = work_a.path().join("proj-a");
    std::fs::create_dir_all(&proj_a).unwrap();
    ferry_cli::commands::init::run(&proj_a).unwrap();

    let out_a = ferry_cli::commands::share::run(&proj_a, false, 5)
        .expect("share should succeed with code path");
    assert!(out_a.json["code"].is_string());
    let code = out_a.json["code"].as_str().unwrap().to_string();
    let folder_id_a = out_a.json["folder_id"].as_str().unwrap().to_string();
    assert_eq!(folder_id_a.len(), 32);

    let _has_offer = proj_a.join(".ferry/pair-offer.ferry-pair").exists();

    std::env::set_var("FERRY_HOME", home_b.path());
    let dest_b = work_b.path().join("proj-b");
    std::fs::create_dir_all(&dest_b).unwrap();

    let out_b = ferry_cli::commands::join::run(&code, Some(&dest_b)).expect("join should succeed");
    assert_eq!(out_b.json["folder_id"], folder_id_a);
    assert_eq!(out_b.json["status"], "joined");
    assert!(dest_b.join(".ferry").is_dir());
    assert!(dest_b.join(".ferry/config").is_file());

    let cfg_bytes = std::fs::read(dest_b.join(".ferry/config")).unwrap();
    let head = ferry_crypto::config_head::parse_config_head(&cfg_bytes).unwrap();
    assert_eq!(ferry_store::format::hex(&head.folder_id), folder_id_a);

    let id_a =
        ferry_crypto::identity::load_or_create(&ferry_cli::home::identity_root(home_a.path()))
            .unwrap();
    let id_b =
        ferry_crypto::identity::load_or_create(&ferry_cli::home::identity_root(home_b.path()))
            .unwrap();

    let cfg_a = std::fs::read(proj_a.join(".ferry/config")).unwrap();
    let head_a = ferry_crypto::config_head::parse_config_head(&cfg_a).unwrap();
    assert_eq!(
        head_a.entries.len(),
        2,
        "A's config must have 2 device wraps"
    );
    let pubs_a: Vec<_> = head_a.entries.iter().map(|e| e.device_pub).collect();
    assert!(pubs_a.contains(id_a.public()));
    assert!(pubs_a.contains(id_b.public()));

    let cfg_b = std::fs::read(dest_b.join(".ferry/config")).unwrap();
    let head_b = ferry_crypto::config_head::parse_config_head(&cfg_b).unwrap();
    assert_eq!(
        head_b.entries.len(),
        2,
        "B's config must have 2 device wraps"
    );
    let pubs_b: Vec<_> = head_b.entries.iter().map(|e| e.device_pub).collect();
    assert!(pubs_b.contains(id_a.public()));
    assert!(pubs_b.contains(id_b.public()));

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
    ferry_cli::commands::init::run(&proj).unwrap();
    let out = ferry_cli::commands::share::run(&proj, false, 5).unwrap();
    assert!(out.json["code"].is_string());
    assert!(out.json["expires_at"].is_string());

    assert!(out.human.contains("Share code"));
    std::env::remove_var("FERRY_HOME");
}

#[test]
fn clean_environment_autostarts_daemon_and_status_stop_work() {
    let home = temp_home();
    let work = tempfile::tempdir().unwrap();
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("FERRY_HOME", home.path());

    let proj = work.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    ferry_cli::commands::init::run(&proj).unwrap();

    // Verify initially no daemon is running
    let status_before = ferry_cli::commands::daemon::status_in(home.path()).unwrap();
    assert_eq!(status_before.json["status"], "stopped");

    // Running share should auto-spawn the daemon in the background
    let out = ferry_cli::commands::share::run(&proj, false, 5).unwrap();
    assert!(out.json["code"].is_string());

    // Verify daemon is now running and recorded with pid
    let status_running = ferry_cli::commands::daemon::status_in(home.path()).unwrap();
    assert_eq!(status_running.json["status"], "running");
    let spawned_pid = status_running.json["pid"].as_u64().expect("daemon pid");
    assert!(spawned_pid > 0);

    // Running pin start reuses the already running daemon
    let pin_out = ferry_cli::commands::pin::start(&proj, &["src/**".to_string()], 1).unwrap();
    assert_eq!(pin_out.json["command"], "pin");

    // ferry daemon stop successfully terminates the auto-spawned daemon
    let stop_out = ferry_cli::commands::daemon::stop_in(home.path()).unwrap();
    assert_eq!(stop_out.json["status"], "stopped");
    assert_eq!(stop_out.json["pid"].as_u64(), Some(spawned_pid));

    // Verify status is now stopped
    let status_after = ferry_cli::commands::daemon::status_in(home.path()).unwrap();
    assert_eq!(status_after.json["status"], "stopped");

    std::env::remove_var("FERRY_HOME");
}
