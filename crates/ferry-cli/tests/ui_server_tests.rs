use std::net::SocketAddr;
use std::sync::Arc;

use ferry_cli::commands::ui::{self, UiArgs, UiServerState};
use ferry_ipc::protocol::{DaemonMessage, EngineSnapshot, PinView, ScanStatsView};

mod common;

async fn send_http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, serde_json::Value, String) {
    use std::fmt::Write as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("tcp connect to ui server");

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        let _ = write!(req, "{k}: {v}\r\n");
    }
    if let Some(b) = body {
        let _ = write!(req, "Content-Length: {}\r\n", b.len());
        req.push_str("Content-Type: application/json\r\n");
        req.push_str("\r\n");
        req.push_str(b);
    } else {
        req.push_str("\r\n");
    }

    stream
        .write_all(req.as_bytes())
        .await
        .expect("write http request");

    let mut res = Vec::new();
    stream
        .read_to_end(&mut res)
        .await
        .expect("read http response");
    let res_str = String::from_utf8_lossy(&res).to_string();

    let status = res_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let body_str = if let Some(idx) = res_str.find("\r\n\r\n") {
        &res_str[idx + 4..]
    } else {
        ""
    };

    let json_val = serde_json::from_str(body_str).unwrap_or(serde_json::Value::Null);
    (status, json_val, body_str.to_string())
}

#[test]
fn test_ui_test_flag_and_random_port() {
    let env = common::Env::new("ui_test_flag");
    let work = env.work();

    // Initialize folder first
    ferry_cli::commands::init::run(&work, "init").expect("init");

    let out = ui::run(UiArgs {
        folder: Some(&work),
        gui: false,
        web: true,
        tui: false,
        host: "127.0.0.1",
        port: 0,
        no_open: true,
        test: true,
    })
    .expect("ui run --test");

    assert_eq!(out.json["command"], "ui");
    assert_eq!(out.json["status"], "ok");
    let port = out.json["port"].as_u64().expect("port as u64");
    assert!(port > 0, "port should be randomly assigned > 0");

    let token = out.json["token"].as_str().expect("token string");
    assert_eq!(token.len(), 32, "token must be 32 hex chars");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    let url = out.json["url"].as_str().expect("url string");
    assert_eq!(url, format!("http://127.0.0.1:{port}/?token={token}"));
}

#[tokio::test]
async fn test_token_auth_enforcement_and_static_assets() {
    let env = common::Env::new("token_auth");
    let work = env.work();
    ferry_cli::commands::init::run(&work, "init").expect("init");

    let token = ui::generate_token();
    let state = Arc::new(UiServerState::new(work.clone(), token.clone()));
    let app = ui::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // 1. Static assets should be accessible without token
    let (status, _, body) = send_http(addr, "GET", "/", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("<!doctype html>"));

    let (status, _, body) = send_http(addr, "GET", "/style.css", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("--bg"));

    let (status, _, body) = send_http(addr, "GET", "/app.js", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("loadStatus"));

    // 2. API endpoints without token -> 403 Forbidden
    let (status, json, _) = send_http(addr, "GET", "/api/status", &[], None).await;
    assert_eq!(status, 403);
    assert_eq!(json["code"], "forbidden");

    let (status, json, _) = send_http(addr, "GET", "/api/conflicts", &[], None).await;
    assert_eq!(status, 403);
    assert_eq!(json["code"], "forbidden");

    // 3. API endpoints with wrong token -> 403 Forbidden
    let (status, json, _) =
        send_http(addr, "GET", "/api/status?token=wrong_token", &[], None).await;
    assert_eq!(status, 403);
    assert_eq!(json["code"], "forbidden");

    let (status, json, _) = send_http(
        addr,
        "GET",
        "/api/status",
        &[("Authorization", "Bearer badtoken1234567890123456789012")],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(json["code"], "forbidden");

    // 4. API endpoints with valid query param token -> 200 OK
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/status?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "status");

    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/conflicts?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "conflicts");

    // 5. API endpoints with valid Authorization header -> 200 OK
    let auth_header = format!("Bearer {token}");
    let (status, json, _) = send_http(
        addr,
        "GET",
        "/api/status",
        &[("Authorization", &auth_header)],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "status");

    // 6. Unknown API endpoint -> 404
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/unknown_endpoint?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(json["code"], "not-found");

    // 7. /api/events -> 200 text/event-stream with initial event: state
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sse_stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("tcp connect for sse");
    let sse_req = format!("GET /api/events?token={token} HTTP/1.1\r\nHost: {addr}\r\n\r\n");
    sse_stream
        .write_all(sse_req.as_bytes())
        .await
        .expect("write sse req");
    let mut total_str = String::new();
    let mut buf = vec![0u8; 1024];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let n = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            sse_stream.read(&mut buf),
        )
        .await
        .expect("read timeout")
        .expect("read chunk");
        if n == 0 {
            break;
        }
        total_str.push_str(&String::from_utf8_lossy(&buf[..n]));
        if total_str.contains("event: state") {
            break;
        }
    }
    assert!(total_str.starts_with("HTTP/1.1 200 OK"));
    assert!(total_str.contains("content-type: text/event-stream"));
    assert!(total_str.contains("event: state"));
    assert!(total_str.contains(r#""command":"status""#));
    drop(sse_stream);

    server_task.abort();
}

#[tokio::test]
#[allow(deprecated)]
async fn test_endpoint_proxying_over_ipc() {
    let env = common::Env::new("endpoint_ipc");
    let work = env.work();
    ferry_cli::commands::init::run(&work, "init").expect("init");

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&work);

    // Mock IPC Server that broadcasts an initial snapshot and answers commands
    #[cfg(unix)]
    let server = ferry_ipc::transport::unix::IpcServer::bind(&socket_path).expect("bind ipc");
    #[cfg(windows)]
    let server = ferry_ipc::transport::windows::IpcServer::bind(&socket_path).expect("bind ipc");

    let ipc_task = tokio::spawn(async move {
        while let Ok(mut conn) = server.accept().await {
            tokio::spawn(async move {
                let mut snap = EngineSnapshot::new(
                    "/mock/folder",
                    "0123456789abcdef0123456789abcdef",
                    "mock_device_id_hex",
                    "idle",
                );
                snap.manifest_id = Some("manifest_from_ipc".to_string());
                snap.scanned = ScanStatsView::new(42, 7, 1, 1024);
                snap.pin = PinView::active(vec!["src/**".to_string()]);
                snap.conflicts = 3;

                // Send initial snapshot
                let _ = conn.send_message(&DaemonMessage::Snapshot(snap)).await;

                while let Ok(Some(cmd)) = conn.recv_command().await {
                    match cmd {
                        ferry_ipc::protocol::ClientCommand::GetStatus => {
                            // Mirror the real daemon: GetStatus answers with a
                            // FULL snapshot (same shape as the initial one).
                            let mut snap = EngineSnapshot::new(
                                "/mock/folder",
                                "0123456789abcdef0123456789abcdef",
                                "mock_device_id_hex",
                                "idle",
                            );
                            snap.manifest_id = Some("manifest_from_ipc".to_string());
                            snap.scanned = ScanStatsView::new(42, 7, 1, 1024);
                            snap.conflicts = 3;
                            let _ = conn.send_message(&DaemonMessage::Snapshot(snap)).await;
                        }
                        ferry_ipc::protocol::ClientCommand::ListConflicts => {
                            let conflicts_json = serde_json::json!([{
                                "ts": "2026-08-26T22:00:00Z",
                                "folder_id": "0123456789abcdef",
                                "path": "src/conflict.rs",
                                "kind": "modify-modify",
                                "winner": { "device": "dev1" },
                                "loser": { "device": "dev2" },
                            }]);
                            let _ = conn
                                .send_message(&DaemonMessage::Ack {
                                    command: "list_conflicts".to_string(),
                                    message: Some(conflicts_json.to_string()),
                                })
                                .await;
                        }
                        ferry_ipc::protocol::ClientCommand::StartPin { paths, .. } => {
                            let _ = conn
                                .send_message(&DaemonMessage::Ack {
                                    command: "start_pin".to_string(),
                                    message: Some(format!("pinned {} paths", paths.len())),
                                })
                                .await;
                        }
                        ferry_ipc::protocol::ClientCommand::ReleasePin => {
                            let _ = conn
                                .send_message(&DaemonMessage::Ack {
                                    command: "release_pin".to_string(),
                                    message: Some("pin released".to_string()),
                                })
                                .await;
                        }
                        _ => {}
                    }
                }
            });
        }
    });

    let token = ui::generate_token();
    let state = Arc::new(UiServerState::new(work.clone(), token.clone()));
    let app = ui::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // Query status over IPC:
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/status?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "status");
    assert_eq!(json["folder"], "/mock/folder");
    assert_eq!(json["manifest_id"], "manifest_from_ipc");
    assert_eq!(json["scanned"]["files"], 42);
    assert_eq!(json["conflicts"], 3);

    // Query conflicts over IPC:
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/conflicts?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "conflicts");
    let entries = json["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], "src/conflict.rs");

    // Pin start over IPC:
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pin/start?token={token}"),
        &[],
        Some(r#"{"paths": ["src/**", "tests/**"]}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pin");
    assert_eq!(json["action"], "start");

    // Pin release over IPC:
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pin/release?token={token}"),
        &[],
        Some("{}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pin");
    assert_eq!(json["action"], "release");

    server_task.abort();
    ipc_task.abort();
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test]
async fn test_endpoint_disk_fallback_when_daemon_offline() {
    let env = common::Env::new("disk_fallback");
    let work = env.work();
    ferry_cli::commands::init::run(&work, "init").expect("init");

    let token = ui::generate_token();
    let state = Arc::new(UiServerState::new(work.clone(), token.clone()));
    let app = ui::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // 1. Status should succeed via disk fallback
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/status?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "status");
    assert_eq!(json["conflicts"], 0);

    // 2. Conflicts should succeed via disk fallback (empty)
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/conflicts?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "conflicts");
    assert_eq!(json["entries"], serde_json::json!([]));

    // 3. Pin start on disk
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pin/start?token={token}"),
        &[],
        Some(r#"{"paths": ["src/**"]}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pin");
    assert_eq!(json["action"], "start");

    // 4. Pin stop on disk
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pin/stop?token={token}"),
        &[],
        Some(r"{}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pin");
    assert_eq!(json["action"], "stop");

    server_task.abort();
}

#[tokio::test]
async fn test_api_status_peer_agreement_when_nodes_synchronize_and_diverge() {
    let env = common::Env::new("status_peer_agreement");
    let base = env.work();

    let tree_a = base.join("a/tree");
    let store_a = base.join("a/store");
    let tree_b = base.join("b/tree");
    let store_b = base.join("b/store");

    std::fs::create_dir_all(&tree_a).unwrap();
    std::fs::create_dir_all(&store_a).unwrap();
    std::fs::create_dir_all(&tree_b).unwrap();
    std::fs::create_dir_all(&store_b).unwrap();

    std::fs::write(tree_a.join("hello.txt"), b"initial content from node a").unwrap();

    let identity_a = ferry_crypto::identity::DeviceIdentity::from_secret_bytes(&[1u8; 32]);
    let identity_b = ferry_crypto::identity::DeviceIdentity::from_secret_bytes(&[2u8; 32]);
    let dev_a_hex = ferry_store::format::hex(identity_a.public());
    let dev_b_hex = ferry_store::format::hex(identity_b.public());

    let mut cfg_a = ferry_sync::EngineConfig::default_for_test(12345);
    cfg_a.tag = "node-a".into();
    cfg_a.store_dir = store_a.clone();
    cfg_a.tree_dir = tree_a.clone();
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());

    let mut engine_a =
        ferry_sync::SyncEngine::new(cfg_a.clone(), Arc::new(ferry_sync::TcpTransport)).unwrap();
    engine_a.set_identity(identity_a.clone());
    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    let mut cfg_b = ferry_sync::EngineConfig::default_for_test(12345);
    cfg_b.tag = "node-b".into();
    cfg_b.store_dir = store_b.clone();
    cfg_b.tree_dir = tree_b.clone();
    cfg_b.connect_to = Some(addr_a);

    let mut engine_b =
        ferry_sync::SyncEngine::new(cfg_b.clone(), Arc::new(ferry_sync::TcpTransport)).unwrap();
    engine_b.set_identity(identity_b.clone());
    let handle_b = engine_b.start();

    // Wait for nodes to synchronize
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut synced = false;
    while std::time::Instant::now() < deadline {
        if let (Some(a), Some(b)) = (handle_a.agreed_id(), handle_b.agreed_id()) {
            if a == b && a != [0u8; 32] && tree_b.join("hello.txt").exists() {
                synced = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(synced, "nodes failed to synchronize within deadline");

    // Spawn UI server for Node A
    let token_a = "0123456789abcdef0123456789abcdef";
    let state_a = Arc::new(ferry_daemon::ui::UiState::new(
        handle_a.clone(),
        store_a.clone(),
        tree_a.clone(),
        cfg_a.folder_id,
        identity_a.clone(),
    ));
    let backend_a = Arc::new(ferry_daemon::ui::DirectBackend::new(state_a));
    let server_a = ferry_daemon::ui::DashboardServer::new(backend_a).with_token(token_a);
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ui_addr_a = listener_a.local_addr().unwrap();
    let srv_task_a = tokio::spawn(async move {
        axum::serve(listener_a, server_a.router()).await.unwrap();
    });

    // Spawn UI server for Node B
    let token_b = "fedcba9876543210fedcba9876543210";
    let state_b = Arc::new(ferry_daemon::ui::UiState::new(
        handle_b.clone(),
        store_b.clone(),
        tree_b.clone(),
        cfg_b.folder_id,
        identity_b.clone(),
    ));
    let backend_b = Arc::new(ferry_daemon::ui::DirectBackend::new(state_b));
    let server_b = ferry_daemon::ui::DashboardServer::new(backend_b).with_token(token_b);
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ui_addr_b = listener_b.local_addr().unwrap();
    let srv_task_b = tokio::spawn(async move {
        axum::serve(listener_b, server_b.router()).await.unwrap();
    });

    // Query /api/status on Node A
    let (status_code_a, json_a, _) = send_http(
        ui_addr_a,
        "GET",
        &format!("/api/status?token={token_a}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status_code_a, 200);

    // Query /api/status on Node B
    let (status_code_b, json_b, _) = send_http(
        ui_addr_b,
        "GET",
        &format!("/api/status?token={token_b}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status_code_b, 200);

    let manifest_a = json_a["manifest_id"].as_str().expect("manifest_id on A");
    let manifest_b = json_b["manifest_id"].as_str().expect("manifest_id on B");

    // Both nodes report matching signed manifest identifiers
    assert_eq!(
        manifest_a, manifest_b,
        "manifest_id should match across synchronized nodes"
    );
    assert!(!manifest_a.is_empty());

    // Check Peer rows on Node A
    let peers_a = json_a["peers"].as_array().expect("peers array on A");
    assert_eq!(peers_a.len(), 1);
    assert_eq!(peers_a[0]["device_id"], dev_b_hex);
    let peer_b_agreed_manifest = peers_a[0]["last_agreed_manifest_id"]
        .as_str()
        .expect("last_agreed_manifest_id");
    assert_eq!(
        peer_b_agreed_manifest, manifest_a,
        "peer last_agreed_manifest_id must match local manifest_id when agreed"
    );

    // Check Peer rows on Node B
    let peers_b = json_b["peers"].as_array().expect("peers array on B");
    assert_eq!(peers_b.len(), 1);
    assert_eq!(peers_b[0]["device_id"], dev_a_hex);
    let peer_a_agreed_manifest = peers_b[0]["last_agreed_manifest_id"]
        .as_str()
        .expect("last_agreed_manifest_id");
    assert_eq!(
        peer_a_agreed_manifest, manifest_b,
        "peer last_agreed_manifest_id must match local manifest_id when agreed"
    );

    // Also verify via DaemonState snapshot
    let (tx_a, _) = tokio::sync::broadcast::channel(16);
    let daemon_state_a = ferry_daemon::DaemonState::new(
        handle_a.clone(),
        store_a.clone(),
        tree_a.clone(),
        cfg_a.folder_id,
        identity_a.clone(),
        tx_a,
    );
    let snap_a = daemon_state_a.snapshot();
    assert_eq!(snap_a.manifest_id.as_deref(), Some(manifest_a));
    assert_eq!(
        snap_a.peers[0].last_agreed_manifest_id.as_deref(),
        Some(manifest_a)
    );

    // Now test divergence: introduce a local change on Node A
    std::fs::write(tree_a.join("diverge.txt"), b"divergent content on node A").unwrap();
    // Wait until Node A mints a new manifest
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(cur_m) = handle_a.current_manifest_id() {
            if ferry_store::format::hex(&cur_m) != manifest_a {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let (status_code_div, json_div, _) = send_http(
        ui_addr_a,
        "GET",
        &format!("/api/status?token={token_a}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status_code_div, 200);

    let diverged_manifest = json_div["manifest_id"]
        .as_str()
        .expect("diverged manifest_id");
    assert_ne!(
        diverged_manifest, manifest_a,
        "Node A manifest_id should have changed after edit"
    );

    let peers_div = json_div["peers"].as_array().expect("peers array");
    let peer_b_agreed = peers_div[0]["last_agreed_manifest_id"]
        .as_str()
        .expect("last agreed");
    assert_eq!(peer_b_agreed, manifest_a);
    assert_ne!(
        peer_b_agreed, diverged_manifest,
        "Diverged state: last_agreed_manifest_id must differ from local manifest_id"
    );

    srv_task_a.abort();
    srv_task_b.abort();
}

#[tokio::test]
async fn test_async_pairing_workflow_and_status_polling() {
    let env = common::Env::new("async_pairing");
    let work = env.work();
    ferry_cli::commands::init::run(&work, "init").expect("init");

    let token = ui::generate_token();
    let state = Arc::new(UiServerState::new(work.clone(), token.clone()));
    let app = ui::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // 1. Secret detection without i_know returns 409 and does not write offer file
    let creds_file = work.join("credentials.json");
    std::fs::write(&creds_file, b"{\"client_secret\": \"super_secret_value\"}")
        .expect("write creds");

    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/share?token={token}"),
        &[],
        Some(r#"{"i_know": false}"#),
    )
    .await;
    assert_eq!(status, 409, "should fail with 409 conflict on secret risk");
    assert_eq!(json["code"], "secrets-found");
    let warnings = json["warnings"].as_array().expect("warnings array");
    assert!(!warnings.is_empty());
    let offer_file_path = work.join(".ferry").join("pair-offer.ferry-pair");
    assert!(
        !offer_file_path.exists(),
        "offer file must not be written when secret scan blocks"
    );

    // Remove the credentials file
    let _ = std::fs::remove_file(&creds_file);

    // 2. POST /api/share initiates pairing and returns in < 50ms with status: pending
    let start = std::time::Instant::now();
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/share?token={token}"),
        &[],
        Some(r#"{"i_know": false}"#),
    )
    .await;
    let elapsed = start.elapsed();
    assert_eq!(status, 200);
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "POST /api/share took {elapsed:?}, expected < 50ms"
    );
    assert_eq!(json["command"], "share");
    assert_eq!(json["status"], "pending");
    let short_code = json["short_code"].as_str().expect("short code");
    assert!(!short_code.is_empty(), "short code must not be empty");
    let offer_file = json["offer_file"].as_str().expect("offer file string");
    assert!(
        std::path::Path::new(offer_file).exists(),
        "offer file must be written to disk"
    );

    // 3. GET /api/share/status returns pending before response file exists
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/share/status?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "share");
    assert_eq!(json["status"], "pending");
    assert_eq!(json["short_code"], short_code);

    // 4. Accept the pair from peer device (device B) to write pair-response
    let (_home_b, id_b) = {
        let dir = tempfile::tempdir().unwrap();
        let id = ferry_crypto::identity::load_or_create(&dir.path().join("id_b")).unwrap();
        (dir, id)
    };
    let target_b = tempfile::tempdir().unwrap();
    let pending_b = ferry_folder::pairing::accept_begin(
        &id_b,
        std::path::Path::new(offer_file),
        Some(target_b.path()),
    )
    .expect("accept_begin");
    assert_eq!(pending_b.expected_short_code, short_code);

    // Response file should now exist at <work>/.ferry/pair-response.ferry-pair
    let response_file_path = work.join(".ferry").join("pair-response.ferry-pair");
    assert!(response_file_path.exists());

    // 5. GET /api/share/status transitions to completed once response exists
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/share/status?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "share");
    assert_eq!(json["status"], "completed");
    let peer_device_id = json["peer_device_id"].as_str().expect("peer_device_id");
    assert_eq!(peer_device_id, ferry_store::format::hex(id_b.public()));

    // 6. Completing the accept side on device B with the written grant
    let accepted =
        ferry_folder::pairing::accept_complete(pending_b, &id_b, 5).expect("accept_complete");
    assert_eq!(accepted.folder, target_b.path());

    server_task.abort();
}

#[test]
fn test_ui_gui_and_tui_dispatch_modes() {
    let env = common::Env::new("gui_tui_dispatch");
    let work = env.work();
    ferry_cli::commands::init::run(&work, "init").expect("init");

    // 1. Dispatch --gui in test mode
    let out_gui = ui::run(UiArgs {
        folder: Some(&work),
        gui: true,
        web: false,
        tui: false,
        host: "127.0.0.1",
        port: 0,
        no_open: true,
        test: true,
    })
    .expect("ui --gui --test");
    assert_eq!(out_gui.json["command"], "ui");
    assert_eq!(out_gui.json["frontend"], "gui");
    assert_eq!(out_gui.json["status"], "ok");

    // 2. Dispatch --tui in test mode
    let out_tui = ui::run(UiArgs {
        folder: Some(&work),
        gui: false,
        web: false,
        tui: true,
        host: "127.0.0.1",
        port: 0,
        no_open: true,
        test: true,
    })
    .expect("ui --tui --test");
    assert_eq!(out_tui.json["command"], "ui");
    assert_eq!(out_tui.json["frontend"], "tui");
    assert_eq!(out_tui.json["status"], "ok");

    // 3. Default frontend dispatch in test mode (preferred = gui)
    let out_default = ui::run(UiArgs {
        folder: Some(&work),
        gui: false,
        web: false,
        tui: false,
        host: "127.0.0.1",
        port: 0,
        no_open: true,
        test: true,
    })
    .expect("ui --test (default)");
    assert_eq!(out_default.json["command"], "ui");
    #[cfg(feature = "gui")]
    assert_eq!(out_default.json["frontend"], "gui");
    #[cfg(all(not(feature = "gui"), feature = "web-ui"))]
    assert_eq!(out_default.json["frontend"], "web");
}
