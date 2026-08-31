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

pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn init() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(out);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

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
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = tokio::task::spawn_blocking(move || loop {
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
        });
        Self {
            rx,
            _worker: worker,
        }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}
