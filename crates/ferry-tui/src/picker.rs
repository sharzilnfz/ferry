use std::path::{Path, PathBuf};

use ferry_ipc::backend::{OpError, UiBackend};
use ferry_ipc::fs::DirectoryEntry;

// ── headless helper ──────────────────────────────────────────────────────────

#[must_use]
pub fn is_headless_env(term: &str, is_tty: bool) -> bool {
    if term == "dumb" {
        return true;
    }
    if std::env::var("FERRY_TUI_FORCE_HEADLESS").is_ok() {
        return true;
    }
    if std::env::var("FERRY_TUI_FORCE_TTY").is_ok() {
        return false;
    }
    !is_tty
}

#[must_use]
pub fn is_headless() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    #[cfg(test)]
    {
        if term == "dumb" {
            return true;
        }
        if std::env::var("FERRY_TUI_FORCE_HEADLESS").is_ok() {
            return true;
        }
        false
    }
    #[cfg(not(test))]
    {
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        is_headless_env(&term, is_tty)
    }
}

#[cfg(test)]
pub fn set_headless_override(_v: Option<bool>) {
    // kept for backwards compat; no-op now that headless is per-app
}

#[must_use]
pub fn headless_error() -> OpError {
    OpError::new("no-tty", "no tty available", "pass explicit path")
}

// ── PickerState ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PickerState {
    pub current_path: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub cursor: usize,
    pub filter: String,
    pub loading: bool,
    pub hint: Option<String>,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            current_path: PathBuf::from("/"),
            entries: Vec::new(),
            cursor: 0,
            filter: String::new(),
            loading: false,
            hint: None,
        }
    }

    pub fn open(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            self.current_path = p;
        }
        self.loading = true;
        self.cursor = 0;
        self.filter.clear();
        self.entries.clear();
        self.hint = None;
    }

    pub async fn open_and_load(
        &mut self,
        backend: &dyn UiBackend,
        path: Option<PathBuf>,
    ) -> Result<(), OpError> {
        self.open(path);
        self.load(backend).await
    }

    pub async fn load(&mut self, backend: &dyn UiBackend) -> Result<(), OpError> {
        self.loading = true;
        match backend.list_directory(Some(self.current_path.clone())).await {
            Ok(resp) => {
                self.set_entries(resp.entries, resp.absolute_path);
                Ok(())
            }
            Err(e) => {
                self.loading = false;
                Err(e)
            }
        }
    }

    pub fn set_entries(&mut self, entries: Vec<DirectoryEntry>, absolute_path: PathBuf) {
        self.entries = entries;
        self.current_path = absolute_path;
        self.loading = false;
        self.cursor = 0;
        self.hint = None;
        self.clamp_cursor();
    }

    pub fn move_up(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        if self.cursor == 0 {
            self.cursor = len - 1;
        } else {
            self.cursor -= 1;
        }
        self.hint = None;
    }

    pub fn move_down(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        self.cursor = (self.cursor + 1) % len;
        self.hint = None;
    }

    #[must_use]
    pub fn enter(&self) -> Option<PathBuf> {
        self.selected().and_then(|e| if e.is_dir { Some(e.path.clone()) } else { None })
    }

    pub async fn enter_and_load(&mut self, backend: &dyn UiBackend) -> Result<bool, OpError> {
        if let Some(target) = self.enter() {
            self.open(Some(target));
            self.load(backend).await?;
            return Ok(true);
        }
        Ok(false)
    }

    #[must_use]
    pub fn go_parent(&self) -> Option<PathBuf> {
        let parent = self.current_path.parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        let p = parent.to_path_buf();
        if p == self.current_path {
            return None;
        }
        Some(p)
    }

    pub async fn go_parent_and_load(&mut self, backend: &dyn UiBackend) -> Result<bool, OpError> {
        if let Some(parent) = self.go_parent() {
            self.open(Some(parent));
            self.load(backend).await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn apply_filter(&mut self, s: &str) {
        self.filter = s.to_string();
        self.cursor = 0;
        self.hint = None;
        self.clamp_cursor();
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.cursor = 0;
        self.hint = None;
    }

    #[must_use]
    pub fn selected(&self) -> Option<&DirectoryEntry> {
        let visible = self.visible_entries();
        if visible.is_empty() || self.cursor >= visible.len() {
            return None;
        }
        Some(visible[self.cursor])
    }

    #[must_use]
    pub fn selected_owned(&self) -> Option<DirectoryEntry> {
        self.selected().cloned()
    }

    #[must_use]
    pub fn visible_entries(&self) -> Vec<&DirectoryEntry> {
        if self.filter.is_empty() {
            return self.entries.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&needle))
            .collect()
    }

    #[must_use]
    pub fn visible_len(&self) -> usize {
        if self.filter.is_empty() {
            return self.entries.len();
        }
        let needle = self.filter.to_lowercase();
        self.entries.iter().filter(|e| e.name.to_lowercase().contains(&needle)).count()
    }

    fn clamp_cursor(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    #[must_use]
    pub fn breadcrumbs(&self) -> String {
        self.current_path.display().to_string()
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.cursor = 0;
        self.hint = None;
        self.clamp_cursor();
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.cursor = 0;
        self.hint = None;
        self.clamp_cursor();
    }

    #[must_use]
    pub fn has_filter(&self) -> bool {
        !self.filter.is_empty()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible_len() == 0
    }

    // Selection logic respecting already-synced dimming.
    // Returns Selected entry if eligible, Hint if already-synced, Nothing otherwise.
    #[must_use]
    pub fn try_select(&mut self) -> PickerSelectResult {
        let Some(entry) = self.selected_owned() else {
            return PickerSelectResult::Nothing;
        };
        if !entry.is_dir {
            return PickerSelectResult::Nothing;
        }
        if entry.is_already_synced {
            self.hint = Some("already synced".to_string());
            return PickerSelectResult::AlreadySynced(entry);
        }
        self.hint = None;
        PickerSelectResult::Selected(entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerSelectResult {
    Selected(DirectoryEntry),
    AlreadySynced(DirectoryEntry),
    Nothing,
}

/// Helper for path parent independent of state, used in app.
#[must_use]
pub fn parent_of(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    let p = parent.to_path_buf();
    if p == path {
        return None;
    }
    Some(p)
}
