//! High-level `TuiApp` state machine and asynchronous event loop.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ferry_ipc::framing::{IpcConnection, IpcReceiver, IpcSender};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage};
use ratatui::backend::Backend;
use ratatui::{Frame, Terminal};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::TuiError;
use crate::state::{SyncState, TuiState};
use crate::terminal::TerminalEvents;
use crate::timefmt::current_time_str;
use crate::ui;

/// Main TUI Application state machine managing incoming IPC messages,
/// keyboard input, and rendering to the active terminal.
pub struct TuiApp {
    pub state: TuiState,
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
        Self { state }
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

    /// Process an incoming server push message from the daemon.
    pub fn handle_message(&mut self, msg: DaemonMessage) {
        self.state.handle_daemon_message(msg);
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
                    Some(ClientCommand::StartPin { paths: Vec::new() })
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

    /// Run the primary asynchronous event loop over an IPC duplex connection and terminal event stream.
    ///
    /// Wakes only on incoming IPC messages or terminal key events with zero idle CPU overhead.
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
        // Request initial status snapshot on connection startup
        if let Err(e) = sender.send_command(&ClientCommand::GetStatus).await {
            self.state.activity_log.push_warn(
                current_time_str(),
                format!("Failed to request initial snapshot: {e}"),
            );
        }

        // Initial frame draw
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
                            // Daemon cleanly disconnected / EOF
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
