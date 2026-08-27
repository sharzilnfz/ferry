use ferry_ipc::{
    create_in_memory_pair, ClientCommand, DaemonMessage, EngineSnapshot, ScanStatsView,
};

#[tokio::test]
async fn test_in_memory_high_throughput_ordering() {
    let (mut client, mut server) = create_in_memory_pair();

    let count = 500;

    let send_handle = tokio::spawn(async move {
        for i in 0..count {
            client
                .send_command(&ClientCommand::StartPin {
                    paths: vec![format!("path_{}", i)],
                    duration_hours: None,
                })
                .await
                .expect("send failed");
        }
    });

    let recv_handle = tokio::spawn(async move {
        for i in 0..count {
            let msg = server
                .recv_command()
                .await
                .expect("recv failed")
                .expect("expected msg");
            match msg {
                ClientCommand::StartPin { paths, .. } => {
                    assert_eq!(paths, vec![format!("path_{i}")]);
                }
                _ => panic!("unexpected command"),
            }
        }
    });

    let (send_res, recv_res) = tokio::join!(send_handle, recv_handle);
    send_res.unwrap();
    recv_res.unwrap();
}

#[tokio::test]
async fn test_in_memory_reconnect_simulation() {
    // Session 1
    {
        let (mut client1, mut server1) = create_in_memory_pair();
        client1.send_command(&ClientCommand::Ping).await.unwrap();
        let cmd = server1.recv_command().await.unwrap().unwrap();
        assert_eq!(cmd, ClientCommand::Ping);
        server1.send_message(&DaemonMessage::Pong).await.unwrap();
        let resp = client1.recv_message().await.unwrap().unwrap();
        assert_eq!(resp, DaemonMessage::Pong);
        // Client disconnects
        drop(client1);
        let eof = server1.recv_command().await.unwrap();
        assert!(eof.is_none());
    }

    // Session 2 (reconnect)
    {
        let (mut client2, mut server2) = create_in_memory_pair();
        client2
            .send_command(&ClientCommand::GetStatus)
            .await
            .unwrap();
        let cmd = server2.recv_command().await.unwrap().unwrap();
        assert_eq!(cmd, ClientCommand::GetStatus);
        let snap = EngineSnapshot::new("/tmp/test", "f1", "d1", "idle");
        server2
            .send_message(&DaemonMessage::Snapshot(snap.clone()))
            .await
            .unwrap();
        let resp = client2.recv_message().await.unwrap().unwrap();
        assert_eq!(resp, DaemonMessage::Snapshot(snap));
    }
}

#[cfg(unix)]
mod unix_tests {
    use super::*;
    use ferry_ipc::{IpcClient, IpcServer};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_unix_domain_socket_roundtrip() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("daemon.sock");

        let server = IpcServer::bind(&sock_path).expect("server bind failed");
        assert!(sock_path.exists(), "socket file must exist after bind");

        let server_task = tokio::spawn(async move {
            let mut conn = server.accept().await.expect("accept failed");
            let cmd = conn.recv_command().await.unwrap().unwrap();
            assert_eq!(cmd, ClientCommand::GetStatus);

            let mut snap = EngineSnapshot::new("/test/folder", "folder_a", "device_a", "synced");
            snap.scanned = ScanStatsView::new(100, 10, 0, 1024);
            conn.send_message(&DaemonMessage::Snapshot(snap))
                .await
                .unwrap();
        });

        let mut client = IpcClient::connect(&sock_path)
            .await
            .expect("client connect failed");
        client
            .send_command(&ClientCommand::GetStatus)
            .await
            .unwrap();

        let msg = client.recv_message().await.unwrap().unwrap();
        if let DaemonMessage::Snapshot(snap) = msg {
            assert_eq!(snap.folder, "/test/folder");
            assert_eq!(snap.scanned.files, 100);
            assert_eq!(snap.state, "synced");
        } else {
            panic!("Expected Snapshot message");
        }

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_unix_socket_stale_rebind_and_cleanup() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("daemon.sock");

        // First server instance
        {
            let server1 = IpcServer::bind(&sock_path).unwrap();
            assert!(sock_path.exists());
            drop(server1);
            // After drop, socket file should be removed
            assert!(!sock_path.exists(), "socket file should be cleaned on drop");
        }

        // Manually create a stale dummy file at socket path
        std::fs::write(&sock_path, b"stale data").unwrap();
        assert!(sock_path.exists());

        // Second server instance should automatically clean up stale file and succeed
        let server2 = IpcServer::bind(&sock_path).unwrap();
        assert!(sock_path.exists());

        // Connect client to verify it works
        let mut client = IpcClient::connect(&sock_path).await.unwrap();
        client.send_command(&ClientCommand::Ping).await.unwrap();

        let mut conn = server2.accept().await.unwrap();
        let cmd = conn.recv_command().await.unwrap().unwrap();
        assert_eq!(cmd, ClientCommand::Ping);

        server2.close();
        assert!(!sock_path.exists(), "close() should remove socket file");
    }

    #[tokio::test]
    async fn test_unix_multiple_concurrent_clients() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("daemon.sock");

        let server = IpcServer::bind(&sock_path).unwrap();

        let server_task = tokio::spawn(async move {
            for i in 0..5 {
                let mut conn = server.accept().await.unwrap();
                let cmd = conn.recv_command().await.unwrap().unwrap();
                assert_eq!(cmd, ClientCommand::Ping);
                conn.send_message(&DaemonMessage::Ack {
                    command: "ping".to_string(),
                    message: Some(format!("client_{i}")),
                })
                .await
                .unwrap();
            }
        });

        let mut client_tasks = Vec::new();
        for _ in 0..5 {
            let path = sock_path.clone();
            client_tasks.push(tokio::spawn(async move {
                let mut client = IpcClient::connect(&path).await.unwrap();
                client.send_command(&ClientCommand::Ping).await.unwrap();
                let resp = client.recv_message().await.unwrap().unwrap();
                match resp {
                    DaemonMessage::Ack { command, message } => {
                        assert_eq!(command, "ping");
                        assert!(message.unwrap().starts_with("client_"));
                    }
                    _ => panic!("Expected Ack"),
                }
            }));
        }

        for t in client_tasks {
            t.await.unwrap();
        }
        server_task.await.unwrap();
    }
}
