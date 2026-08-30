use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ferry_daemon::ui::server::{generate_token, DashboardServer};
use ferry_ipc::backend::FakeBackend;
use ferry_ipc::DirectoryEntry;
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

    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
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
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.expect("read");
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

fn make_entry(name: &str, parent: &str, is_dir: bool) -> DirectoryEntry {
    DirectoryEntry {
        name: name.to_string(),
        path: PathBuf::from(format!("{}/{}", parent.trim_end_matches('/'), name)),
        is_dir,
        is_symlink: false,
        is_git_repo: false,
        is_already_synced: false,
        is_initialized: false,
    }
}

async fn spawn_server_with_token() -> (SocketAddr, String, tokio::task::JoinHandle<()>) {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    (addr, token, h)
}

#[tokio::test]
async fn registry_register_rejects_uninitialized_path_with_init_hint() {
    let (addr, token, h) = spawn_server_with_token().await;
    let auth = format!("Bearer {token}");
    let dir = tempfile::tempdir().unwrap();
    let body = serde_json::json!({ "path": dir.path().display().to_string() }).to_string();

    let (status, doc, _) = send_http(
        addr,
        "POST",
        "/api/registry/register",
        &[("Authorization", &auth)],
        Some(&body),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(doc["code"], "not-initialized");
    assert!(doc["hint"].as_str().unwrap().contains("ferry init"));
    assert!(doc["hint"].as_str().unwrap().contains("ferry pair"));

    h.abort();
}

#[tokio::test]
async fn registry_register_delegates_initialized_path_to_backend() {
    let (addr, token, h) = spawn_server_with_token().await;
    let auth = format!("Bearer {token}");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ferry")).unwrap();
    std::fs::write(dir.path().join(".ferry").join("config"), b"head").unwrap();
    let body = serde_json::json!({ "path": dir.path().display().to_string() }).to_string();

    // The guard passes the initialized path through; FakeBackend answers with
    // its own stub error (`not-found` / "not-implemented"), proving the shared
    // inspection did not block it.
    let (status, doc, _) = send_http(
        addr,
        "POST",
        "/api/registry/register",
        &[("Authorization", &auth)],
        Some(&body),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(doc["code"], "not-found");
    assert_eq!(doc["error"], "not-implemented");

    h.abort();
}

#[tokio::test]
async fn registry_register_requires_auth() {
    let (addr, _token, h) = spawn_server_with_token().await;
    let body = serde_json::json!({ "path": "/tmp" }).to_string();

    let (status, doc, _) =
        send_http(addr, "POST", "/api/registry/register", &[], Some(&body)).await;
    assert_eq!(status, 403);
    assert_eq!(doc["code"], "forbidden");

    h.abort();
}

#[tokio::test]
async fn fs_ls_requires_auth() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });

    let (status, body, _) = send_http(addr, "GET", "/api/fs/ls?path=/tmp", &[], None).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/tmp",
        &[("Authorization", "Bearer wrong_token_00000000000000000000")],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    h.abort();
}

#[tokio::test]
async fn fs_ls_returns_200_for_valid_path() {
    let fake = Arc::new(FakeBackend::new());
    let mut fixture: HashMap<PathBuf, Vec<DirectoryEntry>> = HashMap::new();
    fixture.insert(
        PathBuf::from("/tmp"),
        vec![
            make_entry("alpha", "/tmp", true),
            make_entry("beta.txt", "/tmp", false),
            make_entry("projects", "/tmp", true),
        ],
    );
    fake.set_fs_fixture(fixture).await;
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });

    let auth = format!("Bearer {token}");
    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/tmp",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["absolute_path"], "/tmp");
    let entries = body["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 3);
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta.txt"));

    h.abort();
}

#[tokio::test]
async fn fs_ls_path_traversal_returns_403() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=../../etc",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "path-traversal");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/tmp/../etc",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "path-traversal");

    h.abort();
}

#[tokio::test]
async fn fs_ls_encoded_traversal_returns_403() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=%2e%2e%2fetc",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "path-traversal");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=%2E%2E%2Fetc",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "path-traversal");

    h.abort();
}

#[tokio::test]
async fn fs_ls_null_byte_returns_400() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=%00",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "bad-path");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/tmp/%00",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "bad-path");

    h.abort();
}

#[tokio::test]
async fn fs_ls_double_slash_returns_400() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=//tmp",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "bad-path");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/tmp//sub",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "bad-path");

    h.abort();
}

#[tokio::test]
async fn fs_ls_bad_path_non_absolute_returns_400() {
    let fake = Arc::new(FakeBackend::new());
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=relative/path",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "bad-path");

    h.abort();
}

#[tokio::test]
async fn fs_ls_missing_path_defaults_to_ok() {
    let fake = Arc::new(FakeBackend::new());
    let default_root = ferry_ipc::default_listing_root();
    let mut fixture: HashMap<PathBuf, Vec<DirectoryEntry>> = HashMap::new();
    fixture.insert(
        default_root.clone(),
        vec![make_entry(
            "child",
            &default_root.display().to_string(),
            true,
        )],
    );
    fake.set_fs_fixture(fixture).await;
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) =
        send_http(addr, "GET", "/api/fs/ls", &[("Authorization", &auth)], None).await;
    assert_eq!(status, 200);
    assert_eq!(body["absolute_path"], default_root.display().to_string());

    h.abort();
}

#[tokio::test]
async fn fs_ls_autocomplete_filter_via_prefix() {
    let fake = Arc::new(FakeBackend::new());
    let mut fixture: HashMap<PathBuf, Vec<DirectoryEntry>> = HashMap::new();
    fixture.insert(
        PathBuf::from("/home"),
        vec![
            make_entry("projects", "/home", true),
            make_entry("profile.txt", "/home", false),
            make_entry("prototypes", "/home", true),
            make_entry("other", "/home", true),
        ],
    );
    fake.set_fs_fixture(fixture).await;
    let token = generate_token();
    let server = DashboardServer::new(fake).with_token(&token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let h = tokio::spawn(async move {
        server.serve(listener).await.unwrap();
    });
    let auth = format!("Bearer {token}");

    let (status, body, _) = send_http(
        addr,
        "GET",
        "/api/fs/ls?path=/home",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(status, 200);
    let entries = body["entries"].as_array().unwrap();
    let pro_matches: Vec<&str> = entries
        .iter()
        .filter(|e| e["name"].as_str().unwrap().starts_with("pro"))
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(pro_matches.contains(&"projects"));
    assert!(pro_matches.contains(&"prototypes"));
    assert!(pro_matches.contains(&"profile.txt"));
    assert!(!pro_matches.contains(&"other"));

    h.abort();
}
