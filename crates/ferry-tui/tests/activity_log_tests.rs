//! Circular buffer and activity log truncation unit tests.

use ferry_ipc::protocol::{DaemonMessage, EngineSnapshot};
use ferry_tui::activity_log::{ActivityLog, LogEntry, LogLevel};

#[test]
fn test_circular_buffer_capacity_truncation() {
    let mut log = ActivityLog::new(5);
    assert_eq!(log.capacity(), 5);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);

    // Push 3 items
    log.push_info("12:00:01", "Item 1");
    log.push_info("12:00:02", "Item 2");
    log.push_info("12:00:03", "Item 3");
    assert_eq!(log.len(), 3);

    // Push 2 more (at capacity)
    log.push_info("12:00:04", "Item 4");
    log.push_info("12:00:05", "Item 5");
    assert_eq!(log.len(), 5);

    // Push 3 more (should evict items 1, 2, 3)
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
    assert!(log.entries().back().unwrap().message.contains("Loaded state snapshot"));

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
    assert!(log.entries().back().unwrap().message.contains("changed to syncing"));

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
    assert!(log.entries().back().unwrap().message.contains("Transfer complete"));

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
    assert!(log.entries().back().unwrap().message.contains("Conflict recorded"));

    log.record_daemon_message(
        "12:00:04",
        &DaemonMessage::Error {
            code: "ERR_IO".to_string(),
            message: "Disk full".to_string(),
        },
    );
    assert_eq!(log.len(), 5);
    assert!(log.entries().back().unwrap().message.contains("Daemon error [ERR_IO]"));

    log.clear();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}
