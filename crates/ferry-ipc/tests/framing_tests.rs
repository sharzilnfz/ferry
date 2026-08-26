use ferry_ipc::{
    create_in_memory_pair, ClientCommand, DaemonMessage, IpcConnection, IpcError,
};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn test_in_memory_single_message_roundtrip() {
    let (mut client, mut server) = create_in_memory_pair();

    // Client sends command
    client
        .send_command(&ClientCommand::Ping)
        .await
        .expect("send ping failed");

    let received_cmd = server
        .recv_command()
        .await
        .expect("recv command failed")
        .expect("expected command");
    assert_eq!(received_cmd, ClientCommand::Ping);

    // Server sends response
    server
        .send_message(&DaemonMessage::Pong)
        .await
        .expect("send pong failed");

    let received_msg = client
        .recv_message()
        .await
        .expect("recv message failed")
        .expect("expected message");
    assert_eq!(received_msg, DaemonMessage::Pong);
}

#[tokio::test]
async fn test_multiple_messages_in_sequence() {
    let (mut client, mut server) = create_in_memory_pair();

    let commands = vec![
        ClientCommand::GetStatus,
        ClientCommand::Ping,
        ClientCommand::StartPin {
            paths: vec!["foo/bar".to_string()],
        },
        ClientCommand::TriggerScan,
        ClientCommand::ReleasePin,
    ];

    for cmd in &commands {
        client.send_command(cmd).await.expect("send failed");
    }

    for expected in &commands {
        let actual = server
            .recv_command()
            .await
            .expect("recv failed")
            .expect("expected message");
        assert_eq!(&actual, expected);
    }
}

#[tokio::test]
async fn test_fragmented_stream_and_whitespace_tolerance() {
    let (a, b) = tokio::io::duplex(64);
    let mut writer = a;
    let mut receiver = IpcConnection::new(b);

    // Write message in fragments with arbitrary chunk boundaries and blank lines
    tokio::spawn(async move {
        writer.write_all(b"\n\n").await.unwrap();
        writer.write_all(b"{\"command\":\"get_").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        writer.write_all(b"status\"}\n\n").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        writer.write_all(b"{\"command\":\"ping\"}\n").await.unwrap();
    });

    let msg1 = receiver
        .recv_command()
        .await
        .unwrap()
        .expect("expected get_status");
    assert_eq!(msg1, ClientCommand::GetStatus);

    let msg2 = receiver
        .recv_command()
        .await
        .unwrap()
        .expect("expected ping");
    assert_eq!(msg2, ClientCommand::Ping);
}

#[tokio::test]
async fn test_message_size_limit_rejection() {
    let (a, b) = tokio::io::duplex(1024);
    let mut writer = a;
    // Set low max message size of 32 bytes
    let mut receiver = IpcConnection::with_max_message_size(b, 32);

    tokio::spawn(async move {
        let long_line = "{\"command\":\"start_pin\",\"args\":{\"paths\":[\"very_long_path_that_exceeds_the_limit\"]}}\n";
        writer.write_all(long_line.as_bytes()).await.unwrap();
    });

    let err = receiver.recv_command().await.unwrap_err();
    match err {
        IpcError::MessageTooLarge { size, max } => {
            assert!(size > 32);
            assert_eq!(max, 32);
        }
        other => panic!("Expected MessageTooLarge error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_clean_eof_detection() {
    let (client, mut server) = create_in_memory_pair();

    // Drop client to simulate connection close
    drop(client);

    let res = server.recv_command().await.expect("recv should succeed with None on EOF");
    assert!(res.is_none(), "expected None on EOF");
}

#[tokio::test]
async fn test_split_concurrent_tasks() {
    let (client, server) = create_in_memory_pair();

    let (mut client_tx, mut client_rx) = client.split();
    let (mut server_tx, mut server_rx) = server.split();

    // Client task: send 50 commands and receive 50 responses concurrently
    let client_task = tokio::spawn(async move {
        for i in 0..50 {
            client_tx
                .send_command(&ClientCommand::StartPin {
                    paths: vec![format!("file_{}.txt", i)],
                })
                .await
                .unwrap();
        }

        let mut received = 0;
        while let Some(msg) = client_rx.recv_message().await.unwrap() {
            if let DaemonMessage::Ack { command, .. } = msg {
                assert_eq!(command, "start_pin");
                received += 1;
                if received == 50 {
                    break;
                }
            }
        }
        received
    });

    // Server task: receive commands and send acks
    let server_task = tokio::spawn(async move {
        let mut processed = 0;
        while let Some(cmd) = server_rx.recv_command().await.unwrap() {
            if let ClientCommand::StartPin { paths } = cmd {
                assert_eq!(paths.len(), 1);
                server_tx
                    .send_message(&DaemonMessage::Ack {
                        command: "start_pin".to_string(),
                        message: Some(paths[0].clone()),
                    })
                    .await
                    .unwrap();
                processed += 1;
                if processed == 50 {
                    break;
                }
            }
        }
        processed
    });

    let (client_res, server_res) = tokio::join!(client_task, server_task);
    assert_eq!(client_res.unwrap(), 50);
    assert_eq!(server_res.unwrap(), 50);
}
