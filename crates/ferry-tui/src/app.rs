

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ferry_ipc::backend::{UiBackend, UiEvent};
use ferry_ipc::framing::{IpcConnection, IpcReceiver, IpcSender};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage};
use ratatui::backend::Backend;
use ratatui::{Frame, Terminal};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::TuiError;
use crate::picker::{self, PickerSelectResult, PickerState};
use crate::state::{SyncState, TuiState};
use crate::terminal::TerminalEvents;
use crate::ui;
use ferry_platform::time::current_time_str;

enum KeyOutcome {
    Handled,
    Command(ClientCommand),
    OpenPicker,
    PickerEnter(std::path::PathBuf),
    PickerSpace,
}



pub struct TuiApp {
    pub state: TuiState,
    pub backend: Option<Arc<dyn UiBackend>>,
    pub picker: Option<PickerState>,
    
    pub headless_override: Option<bool>,
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new(TuiState::default())
    }
}

impl TuiApp {
    
    #[must_use]
    pub fn new(state: TuiState) -> Self {
        Self {
            state,
            backend: None,
            picker: None,
            headless_override: None,
        }
    }

    
    #[must_use]
    pub fn new_with_backend(backend: Arc<dyn UiBackend>) -> Self {
        Self {
            state: TuiState::default(),
            backend: Some(backend),
            picker: None,
            headless_override: None,
        }
    }

    
    
    
    #[must_use]
    pub fn new_auto(
        socket_path: impl Into<std::path::PathBuf>,
        folder_path: impl Into<Option<std::path::PathBuf>>,
    ) -> Self {
        Self::new_with_backend(Arc::new(ferry_ipc::backend::connect_auto(
            socket_path,
            folder_path,
        )))
    }

    
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn UiBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn is_headless(&self) -> bool {
        if let Some(v) = self.headless_override {
            return v;
        }
        picker::is_headless()
    }

    #[must_use]
    pub fn is_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    
    pub async fn open_picker(
        &mut self,
        backend: &Arc<dyn UiBackend>,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), ferry_ipc::backend::OpError> {
        if self.is_headless() {
            let err = picker::headless_error();
            self.state
                .activity_log
                .push_error(current_time_str(), format!("Picker error: {err}"));
            return Err(err);
        }
        let mut p = PickerState::new();
        p.open(path);
        match p.load(backend.as_ref()).await {
            Ok(()) => {
                self.picker = Some(p);
                Ok(())
            }
            Err(e) => {
                self.state
                    .activity_log
                    .push_error(current_time_str(), format!("Picker load error: {e}"));
                Err(e)
            }
        }
    }

    
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.state.should_quit
    }

    
    pub fn render(&self, frame: &mut Frame) {
        ui::render(&self.state, frame);
        if let Some(ref picker) = self.picker {
            ui::render_picker(picker, frame, frame.area());
        }
    }

    
    pub fn handle_message(&mut self, msg: DaemonMessage) {
        self.state.handle_daemon_message(msg);
    }

    
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
            UiEvent::FolderRegistered { path } => {
                self.state
                    .activity_log
                    .push_info(current_time_str(), format!("Folder registered: {path}"));
            }
            UiEvent::Error { code, message } => self.state.apply_error(code, message),
        }
    }

    fn handle_key_inner(&mut self, key: KeyEvent) -> KeyOutcome {
        if key.kind == KeyEventKind::Release {
            return KeyOutcome::Handled;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char('c' | 'C') = key.code {
                self.state.should_quit = true;
                return KeyOutcome::Handled;
            }
        }
        if self.picker.is_some() {
            match key.code {
                KeyCode::Esc => {
                    let has_filter = self.picker.as_ref().is_some_and(PickerState::has_filter);
                    if has_filter {
                        if let Some(p) = self.picker.as_mut() {
                            p.clear_filter();
                        }
                    } else {
                        self.picker = None;
                    }
                    return KeyOutcome::Handled;
                }
                KeyCode::Up => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_up();
                    }
                    return KeyOutcome::Handled;
                }
                KeyCode::Down => {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_down();
                    }
                    return KeyOutcome::Handled;
                }
                KeyCode::Backspace => {
                    if let Some(p) = self.picker.as_mut() {
                        p.pop_filter_char();
                    }
                    return KeyOutcome::Handled;
                }
                KeyCode::Enter => {
                    if let Some(target) = self.picker.as_ref().and_then(PickerState::enter) {
                        return KeyOutcome::PickerEnter(target);
                    }
                    return KeyOutcome::Handled;
                }
                KeyCode::Char(' ') => {
                    return KeyOutcome::PickerSpace;
                }
                KeyCode::BackTab | KeyCode::Tab => {
                    return KeyOutcome::Handled;
                }
                KeyCode::Left | KeyCode::Right => {
                    return KeyOutcome::Handled;
                }
                KeyCode::Char(c) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                    {
                        if let Some(p) = self.picker.as_mut() {
                            p.push_filter_char(c);
                        }
                    }
                    return KeyOutcome::Handled;
                }
                _ => return KeyOutcome::Handled,
            }
        }
        if self.state.show_conflicts_modal {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'Q' | 'c' | 'C') => {
                    self.state.show_conflicts_modal = false;
                    return KeyOutcome::Handled;
                }
                _ => return KeyOutcome::Handled,
            }
        }
        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => {
                self.state.should_quit = true;
                KeyOutcome::Handled
            }
            KeyCode::Char('a' | 'A' | 'o' | 'O') => {
                if self.is_headless() {
                    let err = picker::headless_error();
                    self.state
                        .activity_log
                        .push_error(current_time_str(), format!("Picker error: {err}"));
                    return KeyOutcome::Handled;
                }
                KeyOutcome::OpenPicker
            }
            KeyCode::Char('p' | 'P') => {
                if self.state.pin.holding || self.state.engine_state == SyncState::Pinned {
                    KeyOutcome::Command(ClientCommand::ReleasePin)
                } else {
                    KeyOutcome::Command(ClientCommand::StartPin {
                        paths: Vec::new(),
                        duration_hours: None,
                    })
                }
            }
            KeyCode::Char('r' | 'R') => KeyOutcome::Command(ClientCommand::TriggerScan),
            KeyCode::Char('c' | 'C') => {
                self.state.show_conflicts_modal = true;
                KeyOutcome::Command(ClientCommand::ListConflicts)
            }
            _ => KeyOutcome::Handled,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ClientCommand> {
        match self.handle_key_inner(key) {
            KeyOutcome::Handled => None,
            KeyOutcome::Command(cmd) => Some(cmd),
            KeyOutcome::OpenPicker => {
                let mut p = PickerState::new();
                p.open(None);
                p.loading = true;
                self.picker = Some(p);
                None
            }
            KeyOutcome::PickerEnter(target) => {
                let mut next = PickerState::new();
                next.open(Some(target));
                next.loading = true;
                self.picker = Some(next);
                None
            }
            KeyOutcome::PickerSpace => {
                let result = self.picker.as_mut().map(PickerState::try_select);
                match result {
                    Some(PickerSelectResult::AlreadySynced(_)) => {
                        let hint = "already synced".to_string();
                        if let Some(p) = self.picker.as_mut() {
                            p.hint = Some(hint.clone());
                        }
                        self.state
                            .activity_log
                            .push_warn(current_time_str(), "already synced");
                    }
                    Some(PickerSelectResult::NotInitialized(entry)) => {
                        let err = ferry_folder::FolderError::not_initialized(&entry.path);
                        let msg = format!("{} — {}", err.message, err.hint);
                        if let Some(p) = self.picker.as_mut() {
                            p.hint = Some(msg.clone());
                        }
                        self.state.activity_log.push_warn(current_time_str(), msg);
                    }
                    Some(PickerSelectResult::Selected(_)) => {
                        self.state.activity_log.push_info(
                            current_time_str(),
                            "folder selected (sync handle pending backend)",
                        );
                    }
                    Some(PickerSelectResult::Nothing) | None => {}
                }
                None
            }
        }
    }

    pub async fn handle_key_action(&mut self, backend: &Arc<dyn UiBackend>, key: KeyEvent) {
        match self.handle_key_inner(key) {
            KeyOutcome::Handled => {}
            KeyOutcome::Command(cmd) => match cmd {
                ClientCommand::TriggerScan => {
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
                ClientCommand::ReleasePin => match backend.release_pin().await {
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
                },
                ClientCommand::StartPin { paths, duration_hours } => {
                    match backend.start_pin(paths, duration_hours).await {
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
                ClientCommand::ListConflicts => match backend.list_conflicts().await {
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
                },
                other => {
                    let _ = other;
                }
            },
            KeyOutcome::OpenPicker => {
                if let Err(e) = self.open_picker(backend, None).await {
                    let _ = e;
                }
            }
            KeyOutcome::PickerEnter(target) => {
                let mut next = PickerState::new();
                next.open(Some(target.clone()));
                match next.load(backend.as_ref()).await {
                    Ok(()) => self.picker = Some(next),
                    Err(e) => {
                        self.state.activity_log.push_error(
                            current_time_str(),
                            format!("Picker load error: {e}"),
                        );
                    }
                }
            }
            KeyOutcome::PickerSpace => {
                let result = self.picker.as_mut().map(PickerState::try_select);
                match result {
                    Some(PickerSelectResult::Selected(entry)) => {
                        if !entry.is_initialized {
                            let err = ferry_folder::FolderError::not_initialized(&entry.path);
                            self.state.activity_log.push_warn(
                                current_time_str(),
                                format!("{} — {}", err.message, err.hint),
                            );
                            return;
                        }
                        match backend.register_folder(entry.path.clone()).await {
                            Ok(rec) => {
                                self.state.activity_log.push_info(
                                    current_time_str(),
                                    format!("Folder registered: {}", rec.path.display()),
                                );
                                self.picker = None;
                            }
                            Err(e) => {
                                self.state.activity_log.push_error(
                                    current_time_str(),
                                    format!("Register folder error: {e}"),
                                );
                            }
                        }
                    }
                    Some(PickerSelectResult::AlreadySynced(_)) => {
                        self.state
                            .activity_log
                            .push_warn(current_time_str(), "already synced");
                    }
                    Some(PickerSelectResult::NotInitialized(entry)) => {
                        let err = ferry_folder::FolderError::not_initialized(&entry.path);
                        self.state.activity_log.push_warn(
                            current_time_str(),
                            format!("{} — {}", err.message, err.hint),
                        );
                    }
                    Some(PickerSelectResult::Nothing) | None => {}
                }
            }
        }
    }

    
    pub async fn picker_go_parent(&mut self, backend: &Arc<dyn UiBackend>) {
        let parent = self.picker.as_ref().and_then(PickerState::go_parent);
        if let Some(par) = parent {
            let mut next = PickerState::new();
            next.open(Some(par));
            if next.load(backend.as_ref()).await.is_ok() {
                self.picker = Some(next);
            }
        }
    }

    
    
    
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        backend: Arc<dyn UiBackend>,
        mut events: TerminalEvents,
    ) -> Result<(), TuiError> {
        
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

        
        let mut stream = backend.subscribe_events().await.ok();

        
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

    
    pub async fn run_with_connection<B: Backend, S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        terminal: &mut Terminal<B>,
        connection: IpcConnection<S>,
        events: TerminalEvents,
    ) -> Result<(), TuiError> {
        let (sender, receiver) = connection.split();
        self.run_loop(terminal, receiver, sender, events).await
    }

    
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
