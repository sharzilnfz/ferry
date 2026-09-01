use ferry_ipc::protocol::{DaemonMessage, EngineSnapshot};
use ferry_tui::activity_log::{ActivityLog, LogEntry, LogLevel};

#[test]
fn test_circular_buffer_capacity_truncation() {
    let mut log = ActivityLog::new(5);
    assert_eq!(log.capacity(), 5);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);

    log.push_info("12:00:01", "Item 1");
    log.push_info("12:00:02", "Item 2");
    log.push_info("12:00:03", "Item 3");
    assert_eq!(log.len(), 3);

    log.push_info("12:00:04", "Item 4");
    log.push_info("12:00:05", "Item 5");
    assert_eq!(log.len(), 5);

    log.push_info("12:00:06", "Item 6");
    log.push_info("12:00:07", "Item 7");
    log.push_info("12:00:08", "Item 8");

    assert_eq!(log.len(), 5);
    let entries: Vec<&LogEntry> = log.entries().iter().collect();
    assert_eq!(entries[0].message, "Item 4");
    assert_eq!(entries[1].message, "Item 5");
    assert_eq!(entries[2].message, "Item 6");
    assert_eq!(entries[3].message, "Item 7");
    assert_eq!(entries[4].message, "Item 8");
}

#[test]
fn test_circular_buffer_100_default_capacity() {
    let mut log = ActivityLog::default();
    assert_eq!(log.capacity(), 100);

    for i in 0..150 {
        log.push_info("12:00:00", format!("Event {i}"));
    }

    assert_eq!(log.len(), 100);
    assert_eq!(log.entries().front().unwrap().message, "Event 50");
    assert_eq!(log.entries().back().unwrap().message, "Event 149");
}

#[test]
fn test_log_level_types() {
    let mut log = ActivityLog::new(10);
    log.push_info("12:00:00", "Info msg");
    log.push_warn("12:00:01", "Warn msg");
    log.push_error("12:00:02", "Error msg");
    log.push_success("12:00:03", "Success msg");

    let entries: Vec<&LogEntry> = log.entries().iter().collect();
    assert_eq!(entries[0].level, LogLevel::Info);
    assert_eq!(entries[1].level, LogLevel::Warn);
    assert_eq!(entries[2].level, LogLevel::Error);
    assert_eq!(entries[3].level, LogLevel::Success);
}

#[test]
fn test_record_daemon_messages() {
    let mut log = ActivityLog::new(10);

    log.record_daemon_message(
        "12:00:00",
        &DaemonMessage::Snapshot(EngineSnapshot::new("/path", "f1", "d1", "synced")),
    );
    assert_eq!(log.len(), 1);
    assert!(log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Loaded state snapshot"));

    log.record_daemon_message(
        "12:00:01",
        &DaemonMessage::StateChanged {
            state: "syncing".to_string(),
            manifest_id: "m1234567890abcdef".to_string(),
            agreed_id: None,
            pending_changes: Some(2),
            stats: None,
        },
    );
    assert_eq!(log.len(), 2);
    assert!(log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("changed to syncing"));

    log.record_daemon_message(
        "12:00:02",
        &DaemonMessage::TransferProgress {
            bytes_transferred: 1000,
            total_bytes: 1000,
            current_path: "file.txt".to_string(),
            chunks_transferred: None,
            total_chunks: None,
            peer_device_id: None,
            direction: None,
        },
    );
    assert_eq!(log.len(), 3);
    assert!(log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Transfer complete"));

    log.record_daemon_message(
        "12:00:03",
        &DaemonMessage::ConflictRecorded {
            path: "a.txt".to_string(),
            conflict_path: "a.conflict".to_string(),
            timestamp: 100,
            quarantined_as: None,
        },
    );
    assert_eq!(log.len(), 4);
    assert!(log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Conflict recorded"));

    log.record_daemon_message(
        "12:00:04",
        &DaemonMessage::Error {
            code: "ERR_IO".to_string(),
            message: "Disk full".to_string(),
        },
    );
    assert_eq!(log.len(), 5);
    assert!(log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Daemon error [ERR_IO]"));

    log.clear();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}

#[test]
fn test_deduplicate_consecutive_identical_messages() {
    let mut log = ActivityLog::new(10);
    log.push_info("12:00:00", "Same message");
    log.push_info("12:00:01", "Same message");
    log.push_info("12:00:02", "Same message");

    assert_eq!(log.len(), 1);
    assert_eq!(log.entries().front().unwrap().message, "Same message");

    log.push_info("12:00:03", "Different message");
    assert_eq!(log.len(), 2);

    log.push_info("12:00:04", "Same message");
    assert_eq!(log.len(), 3);
}

#[test]
fn test_deduplicate_consecutive_disconnect_error_messages() {
    let mut log = ActivityLog::new(10);
    log.push_error("12:00:00", "Daemon is offline");
    log.push_error(
        "12:00:01",
        "Backend event stream unreachable: Connection refused",
    );
    log.push_error("12:00:02", "Backend event stream closed");
    log.push_error("12:00:03", "Daemon disconnected");

    // All consecutive disconnect error messages should be deduplicated to 1
    assert_eq!(log.len(), 1);
    assert_eq!(log.entries().front().unwrap().message, "Daemon is offline");

    // Reconnection info event allows a new disconnect error to be recorded later
    log.push_info("12:00:04", "Connected to daemon event stream");
    assert_eq!(log.len(), 2);

    log.push_error("12:00:05", "Backend event stream closed");
    assert_eq!(log.len(), 3);

    // Another consecutive disconnect error is again deduplicated
    log.push_error("12:00:06", "Daemon is offline");
    assert_eq!(log.len(), 3);
}

#[test]
fn test_single_clean_status_update_on_pin_transition() {
    use ferry_ipc::protocol::{EngineSnapshot, PinView};
    use ferry_tui::state::TuiState;

    let mut state = TuiState::default();
    let initial_snap = EngineSnapshot::new("/home/user", "f1", "d1", "idle");
    state.apply_snapshot(initial_snap);

    let log_len_after_init = state.activity_log.len();

    // Transition to active pin
    let mut pinned_snap = EngineSnapshot::new("/home/user", "f1", "d1", "idle");
    pinned_snap.pin = PinView::active(vec![]);
    state.apply_snapshot(pinned_snap);

    // Exactly 1 new message in the activity log for the pin transition
    assert_eq!(state.activity_log.len(), log_len_after_init + 1);
    assert!(state
        .activity_log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Pin started: active"));

    // Applying same state or ack does not duplicate the status update
    let ack_msg = DaemonMessage::Ack {
        command: "start_pin".to_string(),
        message: Some("pinned 0 path(s)".to_string()),
    };
    state
        .activity_log
        .record_daemon_message("12:00:02", &ack_msg);
    assert_eq!(state.activity_log.len(), log_len_after_init + 1);

    // Release the pin
    let mut released_snap = EngineSnapshot::new("/home/user", "f1", "d1", "idle");
    released_snap.pin = PinView::none();
    state.apply_snapshot(released_snap);

    assert_eq!(state.activity_log.len(), log_len_after_init + 2);
    assert!(state
        .activity_log
        .entries()
        .back()
        .unwrap()
        .message
        .contains("Pin released"));
}
