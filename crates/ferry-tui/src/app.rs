//! High-level `TuiApp` state machine and asynchronous event loop.

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ferry_ipc::backend::{UiBackend, UiEvent};
use ferry_ipc::framing::{IpcConnection, IpcReceiver, IpcSender};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage};
use ratatui::backend::Backend;
use ratatui::{Frame, Terminal};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::TuiError;
use crate::state::{SyncState, TuiState};
use crate::terminal::TerminalEvents;
use crate::ui;
use ferry_platform::time::current_time_str;

/// Main TUI Application state machine managing incoming backend push events,
/// keyboard input, and rendering to the active terminal.
pub struct TuiApp {
    pub state: TuiState,
    pub backend: Option<Arc<dyn UiBackend>>,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new(TuiState::default())
    }
}

impl TuiApp {
    /// Construct a new `TuiApp` wrapping an initial state.
    #[must_use]
    pub fn new(state: TuiState) -> Self {
        Self {
            state,
            backend: None,
        }
    }

    /// Construct a new `TuiApp` initialized with a `UiBackend`.
    #[must_use]
    pub fn new_with_backend(backend: Arc<dyn UiBackend>) -> Self {
        Self {
            state: TuiState::default(),
            backend: Some(backend),
        }
    }

    /// Attach a `UiBackend` to this application instance.
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn UiBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Check if the application has been requested to terminate.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.state.should_quit
    }

    /// Render the current state onto the provided ratatui frame.
    pub fn render(&self, frame: &mut Frame) {
        ui::render(&self.state, frame);
    }

    /// Process an incoming server push message from the daemon (legacy wire protocol).
    pub fn handle_message(&mut self, msg: DaemonMessage) {
        self.state.handle_daemon_message(msg);
    }

    /// Process a typed `UiEvent` push event from the unified `UiBackend`.
    pub fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::State(snapshot) => self.state.apply_snapshot(snapshot),
            UiEvent::StateChanged {
                state,
                manifest_id,
                agreed_id,
                pending_changes,
                stats,
            } => self.state.apply_state_changed(
                state,
                manifest_id,
                agreed_id,
                pending_changes,
                stats,
            ),
            UiEvent::TransferProgress {
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            } => self.state.apply_transfer_progress(
                bytes_transferred,
                total_bytes,
                current_path,
                chunks_transferred,
                total_chunks,
                peer_device_id,
                direction,
            ),
            UiEvent::ConflictRecorded {
                path,
                conflict_path,
                timestamp,
                quarantined_as,
            } => self
                .state
                .apply_conflict_recorded(path, conflict_path, timestamp, quarantined_as),
            UiEvent::Error { code, message } => self.state.apply_error(code, message),
        }
    }

    /// Process a keyboard event, potentially returning a `ClientCommand` to send over IPC.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ClientCommand> {
        // Ignore key releases if terminal emits release events
        if key.kind == KeyEventKind::Release {
            return None;
        }

        // Ctrl+C terminates immediately
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char('c' | 'C') = key.code {
                self.state.should_quit = true;
                return None;
            }
        }

        // When conflict modal is visible, Esc / q / c dismisses it
        if self.state.show_conflicts_modal {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'Q' | 'c' | 'C') => {
                    self.state.show_conflicts_modal = false;
                    return None;
                }
                _ => return None,
            }
        }

        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => {
                self.state.should_quit = true;
                None
            }
            KeyCode::Char('p' | 'P') => {
                if self.state.pin.holding || self.state.engine_state == SyncState::Pinned {
                    Some(ClientCommand::ReleasePin)
                } else {
                    Some(ClientCommand::StartPin {
                        paths: Vec::new(),
                        duration_hours: None,
                    })
                }
            }
            KeyCode::Char('r' | 'R') => Some(ClientCommand::TriggerScan),
            KeyCode::Char('c' | 'C') => {
                self.state.show_conflicts_modal = true;
                Some(ClientCommand::ListConflicts)
            }
            _ => None,
        }
    }

    /// Process a keyboard event directly against the `UiBackend` trait.
    pub async fn handle_key_action(&mut self, backend: &Arc<dyn UiBackend>, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char('c' | 'C') = key.code {
                self.state.should_quit = true;
                return;
            }
        }

        if self.state.show_conflicts_modal {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'Q' | 'c' | 'C') => {
                    self.state.show_conflicts_modal = false;
                    return;
                }
                _ => return,
            }
        }

        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => {
                self.state.should_quit = true;
            }
            KeyCode::Char('r' | 'R') => {
                if let Err(e) = backend.trigger_scan().await {
                    self.state
                        .activity_log
                        .push_error(current_time_str(), format!("Trigger scan error: {e}"));
                } else {
                    self.state
                        .activity_log
                        .push_info(current_time_str(), "Scan triggered");
                }
            }
            KeyCode::Char('p' | 'P') => {
                if self.state.pin.holding || self.state.engine_state == SyncState::Pinned {
                    match backend.release_pin().await {
                        Ok(summary) => {
                            self.state.activity_log.push_info(
                                current_time_str(),
                                format!("Pin released: {}", summary.status),
                            );
                        }
                        Err(e) => {
                            self.state
                                .activity_log
                                .push_error(current_time_str(), format!("Release pin error: {e}"));
                        }
                    }
                } else {
                    match backend.start_pin(Vec::new(), None).await {
                        Ok(record) => {
                            self.state.activity_log.push_info(
                                current_time_str(),
                                format!("Pin started: {}", record.status),
                            );
                        }
                        Err(e) => {
                            self.state
                                .activity_log
                                .push_error(current_time_str(), format!("Start pin error: {e}"));
                        }
                    }
                }
            }
            KeyCode::Char('c' | 'C') => {
                self.state.show_conflicts_modal = true;
                match backend.list_conflicts().await {
                    Ok(entries) => {
                        self.state.conflict_entries = entries;
                        self.state.conflicts =
                            self.state.conflict_entries.len().max(self.state.conflicts);
                        self.state.update_cached_strings();
                    }
                    Err(e) => {
                        self.state
                            .activity_log
                            .push_error(current_time_str(), format!("List conflicts error: {e}"));
                    }
                }
            }
            _ => {}
        }
    }

    /// Run the primary asynchronous event loop against `Arc<dyn UiBackend>` and `UiEventStream`.
    ///
    /// Wakes reactively only on incoming backend events or terminal key inputs with zero polling overhead.
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        backend: Arc<dyn UiBackend>,
        mut events: TerminalEvents,
    ) -> Result<(), TuiError> {
        // Request initial status snapshot
        match backend.get_status().await {
            Ok(snap) => {
                self.state.apply_snapshot(snap);
            }
            Err(e) => {
                self.state
                    .activity_log
                    .push_warn(current_time_str(), format!("Initial status query: {e}"));
            }
        }

        // Subscribe to real-time push event stream
        let mut stream = backend.subscribe_events().await.ok();

        // Initial draw
        terminal.draw(|f| self.render(f))?;

        while !self.state.should_quit {
            tokio::select! {
                event_res = async {
                    if let Some(ref mut st) = stream {
                        st.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match event_res {
                        Ok(event) => {
                            self.handle_ui_event(event);
                            terminal.draw(|f| self.render(f))?;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            self.state.is_connected = false;
                            self.state.engine_state = SyncState::Offline;
                            self.state.activity_log.push_error(
                                current_time_str(),
                                "Backend event stream closed",
                            );
                            terminal.draw(|f| self.render(f))?;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            self.state.activity_log.push_warn(
                                current_time_str(),
                                format!("Event stream lagged ({n} dropped)"),
                            );
                        }
                    }
                }
                event_opt = events.next() => {
                    if let Some(event) = event_opt {
                        match event {
                            Event::Key(key) => {
                                self.handle_key_action(&backend, key).await;
                                terminal.draw(|f| self.render(f))?;
                            }
                            Event::Resize(..) => {
                                terminal.draw(|f| self.render(f))?;
                            }
                            _ => {}
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Run the primary asynchronous event loop over an IPC duplex connection and terminal event stream (legacy wrapper).
    pub async fn run_with_connection<B: Backend, S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        terminal: &mut Terminal<B>,
        connection: IpcConnection<S>,
        events: TerminalEvents,
    ) -> Result<(), TuiError> {
        let (sender, receiver) = connection.split();
        self.run_loop(terminal, receiver, sender, events).await
    }

    /// Run the primary event loop with separate receiver and sender handles.
    pub async fn run_loop<B: Backend, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &mut self,
        terminal: &mut Terminal<B>,
        mut receiver: IpcReceiver<R>,
        mut sender: IpcSender<W>,
        mut events: TerminalEvents,
    ) -> Result<(), TuiError> {
        if let Err(e) = sender.send_command(&ClientCommand::GetStatus).await {
            self.state.activity_log.push_warn(
                current_time_str(),
                format!("Failed to request initial snapshot: {e}"),
            );
        }

        terminal.draw(|f| self.render(f))?;

        while !self.state.should_quit {
            tokio::select! {
                msg_opt = receiver.recv_message() => {
                    match msg_opt {
                        Ok(Some(msg)) => {
                            self.handle_message(msg);
                            terminal.draw(|f| self.render(f))?;
                        }
                        Ok(None) => {
                            self.state.is_connected = false;
                            self.state.engine_state = SyncState::Offline;
                            self.state.activity_log.push_error(
                                current_time_str(),
                                "Daemon disconnected (IPC connection closed)",
                            );
                            terminal.draw(|f| self.render(f))?;
                        }
                        Err(e) => {
                            self.state.activity_log.push_error(
                                current_time_str(),
                                format!("IPC receive error: {e}"),
                            );
                            terminal.draw(|f| self.render(f))?;
                        }
                    }
                }
                event_opt = events.next() => {
                    if let Some(event) = event_opt {
                        match event {
                            Event::Key(key) => {
                                if let Some(cmd) = self.handle_key(key) {
                                    if let Err(e) = sender.send_command(&cmd).await {
                                        self.state.activity_log.push_error(
                                             current_time_str(),
                                             format!("Failed to send command: {e}"),
                                        );
                                    }
                                }
                                terminal.draw(|f| self.render(f))?;
                            }
                            Event::Resize(..) => {
                                terminal.draw(|f| self.render(f))?;
                            }
                            _ => {}
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}
