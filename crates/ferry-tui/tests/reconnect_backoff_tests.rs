use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ferry_ipc::backend::{
    BoxFuture, FakeBackend, InventoryDomain, OpError, PairResult, PinRecord, PinReleaseSummary,
    PinStopSummary, SessionDomain, ShareOffer, ShareStatus, StatusDomain, UiBackend, UiEventStream,
};
use ferry_ipc::protocol::{ConflictEntry, DiscoveredDeviceView, EngineSnapshot};
use ferry_ipc::{
    CreatePairingRequest, CreatePairingResponse, FolderRecord, JoinPairingRequest,
    ListDirectoryResponse,
};
use ferry_tui::app::ReconnectBackoff;
use ferry_tui::state::SyncState;
use ferry_tui::terminal::TerminalEvents;
use ferry_tui::TuiApp;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[test]
fn test_reconnect_backoff_policy_growth_and_reset() {
    let mut backoff =
        ReconnectBackoff::new(Duration::from_millis(100), Duration::from_millis(800), 2);
    assert_eq!(backoff.attempts, 0);

    // Attempt 1: returns 100ms, advances to 200ms
    assert_eq!(backoff.next_delay(), Duration::from_millis(100));
    assert_eq!(backoff.attempts, 1);

    // Attempt 2: returns 200ms, advances to 400ms
    assert_eq!(backoff.next_delay(), Duration::from_millis(200));
    assert_eq!(backoff.attempts, 2);

    // Attempt 3: returns 400ms, advances to 800ms
    assert_eq!(backoff.next_delay(), Duration::from_millis(400));
    assert_eq!(backoff.attempts, 3);

    // Attempt 4: returns 800ms, capped at 800ms
    assert_eq!(backoff.next_delay(), Duration::from_millis(800));
    assert_eq!(backoff.attempts, 4);

    // Attempt 5: stays at max cap 800ms
    assert_eq!(backoff.next_delay(), Duration::from_millis(800));
    assert_eq!(backoff.attempts, 5);

    // Reset restores initial delay and attempts count
    backoff.reset();
    assert_eq!(backoff.attempts, 0);
    assert_eq!(backoff.next_delay(), Duration::from_millis(100));
}

/// A mock `UiBackend` that delegates to `FakeBackend` when online, and simulates offline errors when offline.
#[derive(Clone)]
struct MockOfflineBackend {
    inner: FakeBackend,
    is_online: Arc<AtomicBool>,
    subscribe_call_count: Arc<AtomicUsize>,
}

impl MockOfflineBackend {
    fn new(initially_online: bool) -> Self {
        Self {
            inner: FakeBackend::new(),
            is_online: Arc::new(AtomicBool::new(initially_online)),
            subscribe_call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn set_online(&self, online: bool) {
        self.is_online.store(online, Ordering::SeqCst);
    }
}

impl StatusDomain for MockOfflineBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let online = self.is_online.load(Ordering::SeqCst);
        if online {
            self.inner.get_status()
        } else {
            Box::pin(async {
                Err(OpError::new(
                    "daemon-offline",
                    "daemon is offline",
                    "start daemon",
                ))
            })
        }
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        self.inner.list_conflicts()
    }

    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        self.inner.trigger_scan()
    }

    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        self.subscribe_call_count.fetch_add(1, Ordering::SeqCst);
        let online = self.is_online.load(Ordering::SeqCst);
        if online {
            self.inner.subscribe_events()
        } else {
            Box::pin(async {
                Err(OpError::new(
                    "stream-unreachable",
                    "cannot subscribe to offline daemon",
                    "start daemon",
                ))
            })
        }
    }

    fn list_discovered_devices(&self) -> BoxFuture<'_, Result<Vec<DiscoveredDeviceView>, OpError>> {
        self.inner.list_discovered_devices()
    }
}

impl InventoryDomain for MockOfflineBackend {
    fn list_directory(
        &self,
        path: Option<std::path::PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        self.inner.list_directory(path)
    }

    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>> {
        self.inner.list_folders()
    }

    fn register_folder(
        &self,
        path: std::path::PathBuf,
    ) -> BoxFuture<'_, Result<FolderRecord, OpError>> {
        self.inner.register_folder(path)
    }

    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        self.inner.remove_folder(folder_id)
    }
}

impl SessionDomain for MockOfflineBackend {
    fn start_pin(
        &self,
        paths: Vec<String>,
        hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>> {
        self.inner.start_pin(paths, hours)
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>> {
        self.inner.stop_pin()
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>> {
        self.inner.release_pin()
    }

    fn share_initiate(
        &self,
        folder: Option<std::path::PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>> {
        self.inner.share_initiate(folder, i_know)
    }

    fn share_status(
        &self,
        folder: Option<std::path::PathBuf>,
    ) -> BoxFuture<'_, Result<ShareStatus, OpError>> {
        self.inner.share_status(folder)
    }

    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<std::path::PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        self.inner.pair_accept(code_or_payload, dir)
    }

    fn create_pairing_session(
        &self,
        req: CreatePairingRequest,
    ) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>> {
        self.inner.create_pairing_session(req)
    }

    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        self.inner.join_pairing_session(req)
    }
}

fn make_key(c: char) -> crossterm::event::Event {
    crossterm::event::Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn buffer_to_string(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, y)).expect("cell exists");
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
async fn test_offline_daemon_applies_backoff_and_header_shows_disconnected() {
    let mock = Arc::new(MockOfflineBackend::new(false));
    let trait_backend: Arc<dyn UiBackend> = mock.clone();

    // Fast backoff: 20ms -> 40ms -> 80ms
    let fast_backoff =
        ReconnectBackoff::new(Duration::from_millis(20), Duration::from_millis(80), 2);

    let mut app = TuiApp::new_with_backend(trait_backend.clone()).with_backoff(fast_backoff);

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let events = TerminalEvents::from_channel(rx);

    // Send 'q' key after 90ms, giving enough time for multiple throttled retries
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(90)).await;
        let _ = tx.send(make_key('q'));
    });

    app.run(&mut terminal, trait_backend, events).await.unwrap();

    // Verify header rendered DISCONNECTED banner while offline
    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("DISCONNECTED"),
        "Header must display DISCONNECTED banner when daemon is offline"
    );

    // Verify backoff throttled retry attempts: multiple subscribe attempts occurred
    let retries = mock.subscribe_call_count.load(Ordering::SeqCst);
    assert!(
        retries >= 2,
        "Expected at least 2 throttled retry attempts, got {retries}"
    );

    // Verify consecutive disconnect error messages were deduplicated
    let error_entries = app
        .state
        .activity_log
        .entries()
        .iter()
        .filter(|e| e.level == ferry_tui::LogLevel::Error)
        .count();
    assert_eq!(
        error_entries, 1,
        "Consecutive disconnect error messages should be deduplicated to 1"
    );
}

#[tokio::test]
async fn test_offline_daemon_reconnects_and_clears_disconnected_status() {
    let mock = Arc::new(MockOfflineBackend::new(false));
    let trait_backend: Arc<dyn UiBackend> = mock.clone();

    let fast_backoff =
        ReconnectBackoff::new(Duration::from_millis(15), Duration::from_millis(50), 2);

    let mut app = TuiApp::new_with_backend(trait_backend.clone()).with_backoff(fast_backoff);

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let events = TerminalEvents::from_channel(rx);

    let mock_clone = mock.clone();
    tokio::spawn(async move {
        // Let it start offline and attempt at least one retry
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Bring daemon online
        mock_clone.set_online(true);
        // Wait for reconnect loop to recover
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = tx.send(make_key('q'));
    });

    app.run(&mut terminal, trait_backend, events).await.unwrap();

    assert!(
        app.state.is_connected,
        "State should be connected after daemon came online"
    );
    assert_eq!(app.state.engine_state, SyncState::Idle);

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("IDLE"),
        "Header should display IDLE after recovering"
    );
    assert!(
        !rendered.contains("DISCONNECTED"),
        "Header should not display DISCONNECTED after recovering"
    );

    // Activity log should have recorded the successful connection
    let connected_log = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("Connected to daemon event stream"));
    assert!(
        connected_log,
        "Activity log should contain connected info message"
    );
}
