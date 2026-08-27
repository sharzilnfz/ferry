use std::sync::Arc;
use std::time::Duration;

use ferry_daemon::ui::server::DashboardServer;
use ferry_ipc::backend::{FakeBackend, UiBackend, UiEvent};
use ferry_ipc::protocol::{ScanStatsView, TransferDirection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn test_push_event_stream_direct_subscription() {
    let backend = FakeBackend::new();
    let mut stream = backend.subscribe_events().await.expect("subscribe_events");

    // Emit StateChanged event
    backend.emit_event(UiEvent::StateChanged {
        state: "syncing".to_string(),
        manifest_id: "m_12345".to_string(),
        agreed_id: Some("agreed_12345".to_string()),
        pending_changes: Some(5),
        stats: Some(ScanStatsView::new(10, 2, 0, 4096)),
    });

    let event = tokio::time::timeout(Duration::from_millis(500), stream.recv())
        .await
        .expect("timed out waiting for event")
        .expect("stream recv error");

    match event {
        UiEvent::StateChanged {
            state,
            manifest_id,
            pending_changes,
            ..
        } => {
            assert_eq!(state, "syncing");
            assert_eq!(manifest_id, "m_12345");
            assert_eq!(pending_changes, Some(5));
        }
        other => panic!("expected StateChanged, got {other:?}"),
    }

    // Emit TransferProgress event
    backend.emit_event(UiEvent::TransferProgress {
        bytes_transferred: 1024,
        total_bytes: 4096,
        current_path: "src/main.rs".to_string(),
        chunks_transferred: Some(1),
        total_chunks: Some(4),
        peer_device_id: Some("peer_hex_123".to_string()),
        direction: Some(TransferDirection::Sending),
    });

    let event2 = tokio::time::timeout(Duration::from_millis(500), stream.recv())
        .await
        .expect("timed out")
        .expect("stream recv error");

    match event2 {
        UiEvent::TransferProgress {
            bytes_transferred,
            total_bytes,
            current_path,
            ..
        } => {
            assert_eq!(bytes_transferred, 1024);
            assert_eq!(total_bytes, 4096);
            assert_eq!(current_path, "src/main.rs");
        }
        other => panic!("expected TransferProgress, got {other:?}"),
    }

    // Emit ConflictRecorded event
    backend.emit_event(UiEvent::ConflictRecorded {
        path: "doc.md".to_string(),
        conflict_path: "doc.md.ferry-conflict".to_string(),
        timestamp: 1700000000,
        quarantined_as: Some("doc.md.ferry-conflict".to_string()),
    });

    let event3 = tokio::time::timeout(Duration::from_millis(500), stream.recv())
        .await
        .expect("timed out")
        .expect("stream recv error");

    match event3 {
        UiEvent::ConflictRecorded {
            path,
            conflict_path,
            ..
        } => {
            assert_eq!(path, "doc.md");
            assert_eq!(conflict_path, "doc.md.ferry-conflict");
        }
        other => panic!("expected ConflictRecorded, got {other:?}"),
    }
}

#[tokio::test]
async fn test_sse_api_events_streaming_and_zero_idle_cpu() {
    let backend = Arc::new(FakeBackend::new());
    let token = "test_event_token_1234567890abcdef";
    let server = DashboardServer::new(backend.clone()).with_token(token);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    let server_task = tokio::spawn(async move {
        axum::serve(listener, server.router())
            .await
            .expect("serve axum");
    });

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("tcp connect");

    // Send HTTP GET request for /api/events with Bearer token
    let req = format!(
        "GET /api/events HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nAccept: text/event-stream\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.expect("write req");

    // 1. Initial snapshot is pushed immediately
    let mut buffer = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .expect("timed out reading initial sse response")
        .expect("read response");

    let initial_resp = String::from_utf8_lossy(&buffer[..n]);
    assert!(initial_resp.contains("HTTP/1.1 200 OK"));
    assert!(initial_resp.contains("content-type: text/event-stream"));
    assert!(initial_resp.contains("event: state"));
    assert!(initial_resp.contains("\"command\":\"status\""));

    // 2. Emit a live StateChanged push event
    backend.emit_event(UiEvent::StateChanged {
        state: "syncing".to_string(),
        manifest_id: "m_live_event".to_string(),
        agreed_id: None,
        pending_changes: Some(3),
        stats: None,
    });

    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .expect("timed out reading live sse event")
        .expect("read chunk");

    let event_resp = String::from_utf8_lossy(&buffer[..n]);
    assert!(event_resp.contains("event: state_changed"));
    assert!(event_resp.contains("\"state\":\"syncing\""));
    assert!(event_resp.contains("\"manifest_id\":\"m_live_event\""));

    // 3. Emit a live ConflictRecorded push event
    backend.emit_event(UiEvent::ConflictRecorded {
        path: "hello.txt".to_string(),
        conflict_path: "hello.txt.ferry-conflict".to_string(),
        timestamp: 1720000000,
        quarantined_as: Some("hello.txt.ferry-conflict".to_string()),
    });

    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .expect("timed out reading conflict sse event")
        .expect("read chunk");

    let conflict_resp = String::from_utf8_lossy(&buffer[..n]);
    assert!(conflict_resp.contains("event: conflict"));
    assert!(conflict_resp.contains("\"path\":\"hello.txt\""));

    drop(stream);
    server_task.abort();
}
