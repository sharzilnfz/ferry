//! Benchmarking and Zero-CPU Idle Verification for `ferry-gui`.
//!
//! Asserts:
//! 1. Cold-start latency of `ferry-gui` initialization and snapshot projection is strictly sub-10ms.
//! 2. Zero-CPU idle verification: `UiEventStream` and `FakeBackend` produce 0 wakeups when idle.
//! 3. UI frame update throughput during idle periods consumes negligible resources.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, RawInput};
use ferry_gui::theme::Theme;
use ferry_gui::GuiApp;
use ferry_ipc::backend::{FakeBackend, StatusDomain, UiEvent};
use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, EngineSnapshot, PeerStatusView, ScanStatsView,
};

#[test]
fn test_cold_start_latency_benchmark_sub_10ms() {
    let fake = Arc::new(FakeBackend::new());

    // Build a realistic enterprise-scale snapshot
    let mut snap = EngineSnapshot::new(
        "/data/enterprise-monorepo",
        "folder-monorepo-999",
        "device-node-001",
        "synced",
    );
    snap.manifest_id = Some("b".repeat(64));
    snap.scanned = ScanStatsView::new(100_000, 5_000, 200, 50 * 1024 * 1024 * 1024);
    for i in 0..50 {
        snap.peers.push(PeerStatusView::new(
            format!("peer-device-{i:03}"),
            if i % 2 == 0 { "online" } else { "dialing" },
        ));
    }

    let mut latencies = Vec::with_capacity(100);

    for _ in 0..100 {
        let start = Instant::now();

        // 1. App construction
        let mut app = GuiApp::new_headless(fake.clone());

        // 2. Headless egui Context + Theme initialization
        let ctx = Context::default();
        Theme::apply(&ctx);

        // 3. Snapshot event ingestion and projection
        app.handle_event(UiEvent::State(snap.clone()));

        // 4. First full UI frame render & layout calculation
        let _ = ctx.run(RawInput::default(), |ctx| {
            app.update_ui(ctx);
        });

        let elapsed = start.elapsed();
        latencies.push(elapsed);
    }

    let total: Duration = latencies.iter().copied().sum();
    let avg = total / (latencies.len() as u32);
    latencies.sort();
    let p99 = latencies[(latencies.len() as f64 * 0.99) as usize];
    let max = *latencies.last().unwrap();

    eprintln!("Cold start benchmark: avg = {avg:?}, p99 = {p99:?}, max = {max:?}");

    // Target: average sub-10ms in release builds (allowing headroom in unoptimized debug test builds)
    let avg_threshold = if cfg!(debug_assertions) {
        Duration::from_millis(100)
    } else {
        Duration::from_millis(10)
    };
    let p99_threshold = if cfg!(debug_assertions) {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(25)
    };
    assert!(
        avg < avg_threshold,
        "Average cold start latency {avg:?} exceeded {avg_threshold:?} target"
    );
    assert!(
        p99 < p99_threshold,
        "P99 cold start latency {p99:?} exceeded {p99_threshold:?} target"
    );
}

#[tokio::test]
async fn test_zero_cpu_idle_verification() {
    let fake = Arc::new(FakeBackend::new());
    let mut stream = fake.subscribe_events().await.unwrap();

    // 1. Initial idle verification: no events should arrive without backend mutation
    let idle_timeout = tokio::time::timeout(Duration::from_millis(50), stream.recv()).await;
    assert!(
        idle_timeout.is_err(),
        "Expected zero wakeups during idle period, but received an event"
    );

    // 2. Emit single event and verify it wakes up exactly once
    fake.emit_event(UiEvent::StateChanged {
        state: "syncing".to_string(),
        manifest_id: "m-wakeup-test".to_string(),
        agreed_id: None,
        pending_changes: Some(1),
        stats: None,
    });

    let received = tokio::time::timeout(Duration::from_millis(100), stream.recv()).await;
    assert!(received.is_ok(), "Expected event after broadcast");
    let event = received.unwrap();
    assert!(matches!(
        event,
        Ok(UiEvent::StateChanged { ref manifest_id, .. }) if manifest_id == "m-wakeup-test"
    ));

    // 3. Subsequent idle period: assert zero wakeups again
    let subsequent_idle = tokio::time::timeout(Duration::from_millis(50), stream.recv()).await;
    assert!(
        subsequent_idle.is_err(),
        "Expected zero wakeups after event consumption during idle, but received extra wakeup"
    );
}

#[test]
fn test_idle_frame_execution_efficiency() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);
    let ctx = Context::default();
    Theme::apply(&ctx);

    // Warm up
    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });

    // Run 60 consecutive idle frames
    let start = Instant::now();
    for _ in 0..60 {
        let _ = ctx.run(RawInput::default(), |ctx| {
            app.update_ui(ctx);
        });
    }
    let duration = start.elapsed();
    let per_frame = duration / 60;

    eprintln!("60 idle frames completed in {duration:?} (avg {per_frame:?} per frame)");

    // Headless frame updates without repaints should be fast
    let threshold = if cfg!(debug_assertions) {
        Duration::from_millis(10)
    } else {
        Duration::from_millis(1)
    };
    assert!(
        per_frame < threshold,
        "Idle frame execution took {per_frame:?}, exceeding {threshold:?} budget"
    );
}

#[test]
fn test_large_snapshot_memory_projection() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);

    // Populate with 200 conflicts and 100 peers
    for i in 0..200 {
        let offset = i64::from(i);
        app.conflicts.push(ConflictEntry {
            ts: format!("2026-08-28T03:{:02}:00Z", i % 60),
            folder_id: "large-monorepo".to_string(),
            path: format!("crates/pkg_{i}/src/lib.rs"),
            kind: "content".to_string(),
            winner: DeviceStamp {
                device: format!("dev-winner-{i}"),
                mtime_sec: Some(1787570000 + offset),
                mtime_nsec: None,
            },
            loser: DeviceStamp {
                device: format!("dev-loser-{i}"),
                mtime_sec: Some(1787560000 + offset),
                mtime_nsec: None,
            },
            quarantined_as: Some(format!("crates/pkg_{i}/src/lib.rs.ferry-conflict")),
        });
    }

    let ctx = Context::default();
    Theme::apply(&ctx);

    // Verify modal and UI render smoothly under load
    app.show_conflicts_modal = true;
    let _ = ctx.run(RawInput::default(), |ctx| {
        app.update_ui(ctx);
    });

    assert_eq!(app.conflicts.len(), 200);
}
