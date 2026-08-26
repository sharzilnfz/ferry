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
    let (status, json, _) = send_http(addr, "GET", "/api/status?token=wrong_token", &[], None).await;
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
    let (status, json, _) =
        send_http(addr, "GET", &format!("/api/status?token={token}"), &[], None).await;
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

    // 7. /api/events -> 501 Not Implemented
    let (status, json, _) =
        send_http(addr, "GET", &format!("/api/events?token={token}"), &[], None).await;
    assert_eq!(status, 501);
    assert_eq!(json["code"], "not-implemented");

    server_task.abort();
}

#[tokio::test]
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
                            let mut snap = EngineSnapshot::new(
                                "/mock/folder",
                                "0123456789abcdef0123456789abcdef",
                                "mock_device_id_hex",
                                "idle",
                            );
                            snap.manifest_id = Some("manifest_from_ipc".to_string());
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
                        ferry_ipc::protocol::ClientCommand::StartPin { paths } => {
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
    let (status, json, _) =
        send_http(addr, "GET", &format!("/api/status?token={token}"), &[], None).await;
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
    let (status, json, _) =
        send_http(addr, "GET", &format!("/api/status?token={token}"), &[], None).await;
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
