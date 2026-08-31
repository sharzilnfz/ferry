#![allow(deprecated)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ferry_daemon::ui::backend::{FolderBackend, FsStateSource};
use ferry_daemon::ui::server::{generate_token, DashboardServer};
use ferry_ipc::backend::FakeBackend;
use ferry_ipc::protocol::{ConflictEntry, DeviceStamp};
use serde_json::Value;

async fn send_http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, Value, String) {
    use std::fmt::Write as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to server");

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

    stream.write_all(req.as_bytes()).await.expect("write req");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("read resp");
    let res_str = String::from_utf8_lossy(&buf).to_string();

    let mut status_code = 0;
    let mut body_part = "";
    if let Some(first_line) = res_str.lines().next() {
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() >= 2 {
            status_code = parts[1].parse().unwrap_or(0);
        }
    }
    if let Some(idx) = res_str.find("\r\n\r\n") {
        body_part = &res_str[idx + 4..];
    }

    let json_body: Value = serde_json::from_str(body_part).unwrap_or(Value::Null);
    (status_code, json_body, body_part.to_string())
}

#[tokio::test]
async fn test_dashboard_server_with_fake_backend_full_lifecycle() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake.clone())
        .with_token(&token)
        .with_inactivity_timeout(Duration::from_secs(60));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });

    let (status, _, body) = send_http(addr, "GET", "/", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.to_ascii_lowercase().contains("<!doctype html>"));

    let (status, _, body) = send_http(addr, "GET", "/style.css", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("font-family") || body.contains("background") || body.contains(":root"));

    let (status, _, _body) = send_http(addr, "GET", "/app.js", &[], None).await;
    assert_eq!(status, 200);

    let (status, json, _) = send_http(addr, "GET", "/api/status", &[], None).await;
    assert_eq!(status, 403);
    assert_eq!(json["code"], "forbidden");

    let auth_hdr = format!("Bearer {token}");
    let (status, json, _) = send_http(
        addr,
        "GET",
        "/api/status",
        &[("Authorization", &auth_hdr)],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "status");
    assert_eq!(json["folder"], "/test/folder");

    fake.add_conflict(ConflictEntry {
        ts: "2026-08-28T00:00:00Z".to_string(),
        folder_id: "0123456789abcdef".to_string(),
        path: "src/main.rs".to_string(),
        kind: "content".to_string(),
        winner: DeviceStamp {
            device: "dev1".to_string(),
            mtime_sec: Some(100),
            mtime_nsec: Some(0),
        },
        loser: DeviceStamp {
            device: "dev2".to_string(),
            mtime_sec: Some(90),
            mtime_nsec: Some(0),
        },
        quarantined_as: Some("src/main.sync-conflict.rs".to_string()),
    })
    .await;

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
    assert_eq!(entries[0]["path"], "src/main.rs");

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

    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pin/release?token={token}"),
        &[],
        Some(r"{}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pin");
    assert_eq!(json["action"], "release");

    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/share?token={token}"),
        &[],
        Some(r#"{"i_know": true}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "share");
    assert_eq!(json["status"], "pending");
    assert!(json["short_code"].is_string());
    assert!(json["qr_code"].is_string());

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
    assert!(json["short_code"].is_string());
    assert!(json["qr_code"].is_string());

    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pair/accept?token={token}"),
        &[],
        Some(r#"{"payload_path": "/test/path/offer.ferry-pair"}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pair");
    assert_eq!(json["status"], "paired");

    server_handle.abort();
}

#[tokio::test]
async fn test_one_click_pairing_and_device_discovery() {
    let backend = Arc::new(FakeBackend::new());
    let disc_dev = ferry_ipc::protocol::DiscoveredDeviceView::new(
        "peer-device-discovered-99",
        Some("192.168.1.150:9000".to_string()),
    );
    backend.add_discovered_device(disc_dev).await;

    let token = generate_token();
    let server = DashboardServer::new(backend).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });

    // 1. Enumerate discovered devices
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/devices?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "devices");
    let devices = json["devices"].as_array().expect("devices array");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["device_id"], "peer-device-discovered-99");
    assert_eq!(devices[0]["address"], "192.168.1.150:9000");

    // 2. Enumerate via /api/devices/discovered
    let (status, json, _) = send_http(
        addr,
        "GET",
        &format!("/api/devices/discovered?token={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "devices");
    assert_eq!(json["devices"].as_array().unwrap().len(), 1);

    // 3. Initiate pair directly with discovered device
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pair/device?token={token}"),
        &[],
        Some(r#"{"device_id": "peer-device-discovered-99"}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pair");
    assert_eq!(json["role"], "device");
    assert_eq!(json["target_device_id"], "peer-device-discovered-99");
    assert!(json["code"].is_string());
    assert!(json["short_code"].is_string());
    assert!(json["qr_code"].is_string());
    let pairing_code = json["code"].as_str().unwrap().to_string();

    // 4. Test /api/pair/join with short-code
    let (status, json, _) = send_http(
        addr,
        "POST",
        &format!("/api/pair/join?token={token}"),
        &[],
        Some(&format!(
            r#"{{"code": "{pairing_code}", "target_dir": "/tmp/test-join-folder"}}"#
        )),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json["command"], "pair");
    assert_eq!(json["role"], "join");
    assert_eq!(json["status"], "paired");
    assert_eq!(json["folder"], "/tmp/test-join-folder");

    server_handle.abort();
}

#[tokio::test]
async fn test_dashboard_server_with_auto_backend() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tree_dir = temp.path().join("tree");
    std::fs::create_dir_all(&tree_dir).expect("create dir");

    let identity = ferry_crypto::identity::DeviceIdentity::generate();
    let folder_id = [7u8; 16];
    let mut rng = rand::rngs::StdRng::seed_from_u64(1234);
    use rand::SeedableRng as _;
    let poly = ferry_store::chunker::generate_polynomial(&mut rng);
    let (store, _) =
        ferry_folder::folder::create_folder(&tree_dir, &identity, folder_id, poly).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();

    let settings = ferry_folder::folder::Settings {
        format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&folder_id),
        honor_gitignore: true,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    ferry_folder::folder::save_settings(&tree_dir, &settings).unwrap();

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&tree_dir);
    let fs_src = FsStateSource::new(tree_dir.clone()).with_identity(identity.clone());
    let fb = FolderBackend::from_source(fs_src);
    let auto = ferry_ipc::backend::AutoBackend::new(socket_path)
        .with_fallback(tree_dir.clone())
        .with_fallback_backend(Arc::new(fb));

    let token = generate_token();
    let server = DashboardServer::new(Arc::new(auto)).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });

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
    assert_eq!(json["folder"], tree_dir.display().to_string());

    server_handle.abort();
}
