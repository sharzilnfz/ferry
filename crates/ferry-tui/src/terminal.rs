//! Terminal initialization, cleanup, RAII guards, and asynchronous input event streams.

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, stdout, Stdout};
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// RAII terminal session guard that configures raw mode and alternate screen,
/// automatically restoring the terminal when dropped.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Initialize raw mode, enter alternate screen, and construct the Ratatui Terminal instance.
    pub fn init() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(out);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Access mutable reference to the underlying Ratatui terminal.
    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
        let _ = self.terminal.show_cursor();
    }
}

/// Explicitly restore terminal state (raw mode disabled, leave alternate screen, disable mouse capture, show cursor) to a writer.
pub fn restore_terminal_writer<W: io::Write>(writer: &mut W) -> io::Result<()> {
    let _ = disable_raw_mode();
    let _ = execute!(
        writer,
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::cursor::Show
    );
    Ok(())
}

/// Install a panic hook that resets the terminal back to normal mode before printing panic info.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let mut out = stdout();
        let _ = execute!(
            out,
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
        original_hook(panic_info);
    }));
}

/// Asynchronous crossterm event reader channel.
pub struct TerminalEvents {
    rx: UnboundedReceiver<Event>,
    _worker: tokio::task::JoinHandle<()>,
}

impl Default for TerminalEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalEvents {
    /// Start a dedicated blocking task to poll crossterm events and forward them asynchronously.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = tokio::task::spawn_blocking(move || {
            loop {
                // Poll every 200ms to avoid blocking shutdown indefinitely
                if event::poll(Duration::from_millis(200)).unwrap_or(false) {
                    if let Ok(evt) = event::read() {
                        if tx.send(evt).is_err() {
                            break;
                        }
                    }
                }
                if tx.is_closed() {
                    break;
                }
            }
        });
        Self {
            rx,
            _worker: worker,
        }
    }

    /// Receive next terminal event asynchronously.
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}
