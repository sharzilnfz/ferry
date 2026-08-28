//! Ratatui UI layout and widget rendering for the Ferry TUI dashboard.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Row, Table},
    Frame,
};

use crate::activity_log::LogLevel;
use crate::state::{SyncState, TuiState};

/// Main entry point for drawing the complete Ferry TUI dashboard onto a frame.
pub fn render(state: &TuiState, frame: &mut Frame) {
    let area = frame.area();

    // Guard against excessively tiny terminals
    if area.width < 40 || area.height < 12 {
        let warning = Paragraph::new("Terminal too small for Ferry TUI\n(Minimum size: 40x12)")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, area);
        return;
    }

    // Top-level 4-part vertical layout:
    // [0] Header (4 lines)
    // [1] Main body: Left Storage/Gauge + Right Peers Table (Min 8 lines)
    // [2] Bottom: Recent Activity Log (7 lines)
    // [3] Footer: Keybindings (1 line)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(state, frame, chunks[0]);
    render_main_body(state, frame, chunks[1]);
    render_activity_log(state, frame, chunks[2]);
    render_footer(state, frame, chunks[3]);

    if state.show_conflicts_modal {
        render_conflicts_modal(state, frame, area);
    } else if state.show_folder_picker_modal {
        render_folder_picker_modal(state, frame, area);
    }
}

/// Render the header with folder path, folder ID, device ID, manifest, and engine state badge.
fn render_header(state: &TuiState, frame: &mut Frame, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Ferry Sync Engine ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    if inner_area.height < 2 {
        return;
    }

    let badge_text = state.engine_state.badge_text();
    let badge_style = match state.engine_state {
        SyncState::Synced => Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD),
        SyncState::Syncing => Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        SyncState::Conflict => Style::default()
            .fg(Color::Yellow)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
        SyncState::Pinned => Style::default()
            .fg(Color::Yellow)
            .bg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        SyncState::Idle => Style::default()
            .fg(Color::Black)
            .bg(Color::Gray)
            .add_modifier(Modifier::BOLD),
        SyncState::Error => Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
        SyncState::Offline => Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    };

    let badge_span = Span::styled(format!(" {badge_text} "), badge_style);

    // Line 1: Folder details + state badge right-aligned
    let folder_left = vec![
        Span::styled(" Folder: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(&state.folder, Style::default().fg(Color::White)),
        Span::styled(" (id: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&state.folder_id, Style::default().fg(Color::DarkGray)),
        Span::styled(")", Style::default().fg(Color::DarkGray)),
    ];

    // Compute padding for badge right-alignment
    let left_len: usize = folder_left.iter().map(|s| s.content.len()).sum();
    let badge_len = badge_text.len() + 2; // with spaces
    let total_w = inner_area.width as usize;
    let pad_len = total_w.saturating_sub(left_len + badge_len);

    let mut line1_spans = folder_left;
    if pad_len > 0 {
        line1_spans.push(Span::raw(" ".repeat(pad_len)));
    }
    line1_spans.push(badge_span);

    // Line 2: Device ID and Manifest Hash
    let short_device = truncate_str(&state.device_id, 32);
    let short_manifest = truncate_str(&state.cached_manifest_line, 32);
    let line2_spans = vec![
        Span::styled(" Device: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(short_device, Style::default().fg(Color::Cyan)),
        Span::styled(
            "  │  Manifest: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(short_manifest, Style::default().fg(Color::DarkGray)),
    ];

    let header_lines = vec![Line::from(line1_spans), Line::from(line2_spans)];
    let paragraph = Paragraph::new(header_lines);
    frame.render_widget(paragraph, inner_area);
}

/// Render the split main body (Left: Local Storage & Progress Gauge; Right: Peers Table).
fn render_main_body(state: &TuiState, frame: &mut Frame, area: Rect) {
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(55), // Storage metrics & transfer gauge
            Constraint::Percentage(45), // Peer connectivity table
        ])
        .split(area);

    render_storage_and_progress(state, frame, main_chunks[0]);
    render_peers_table(state, frame, main_chunks[1]);
}

/// Render the left pane containing storage metrics and the chunk transfer progress gauge.
fn render_storage_and_progress(state: &TuiState, frame: &mut Frame, area: Rect) {
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // Storage & State metrics
            Constraint::Length(3), // Transfer Progress Gauge
        ])
        .split(area);

    // Storage block
    let storage_block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Storage & Sync State ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner_storage = storage_block.inner(left_chunks[0]);
    frame.render_widget(storage_block, left_chunks[0]);

    let pin_style = if state.pin.holding {
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let pending_str = match state.pending_changes {
        Some(n) if n > 0 => format!("{n} change(s) pending"),
        Some(0) => "0 (up to date)".to_string(),
        Some(_) => "unknown (base unreadable)".to_string(),
        None => "no agreement yet".to_string(),
    };

    let conflict_style = if state.conflicts > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let metrics_lines = vec![
        Line::from(vec![
            Span::styled(
                " Scanned:    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(&state.cached_metrics_line),
        ]),
        Line::from(vec![
            Span::styled(
                " Manifest:   ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(&state.cached_manifest_line),
        ]),
        Line::from(vec![
            Span::styled(
                " Pin Status: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(&state.cached_pin_line, pin_style),
        ]),
        Line::from(vec![
            Span::styled(
                " Pending:    ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(pending_str),
        ]),
        Line::from(vec![
            Span::styled(
                " Conflicts:  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{}", state.conflicts), conflict_style),
        ]),
    ];

    let storage_paragraph = Paragraph::new(metrics_lines);
    frame.render_widget(storage_paragraph, inner_storage);

    // Progress Gauge widget (zero string allocation per frame)
    let gauge_style = if state.active_transfer.is_some() {
        Style::default().fg(Color::Cyan).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::DarkGray).bg(Color::Black)
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Transfer Progress "),
        )
        .gauge_style(gauge_style)
        .percent(state.cached_progress_percent)
        .label(&state.cached_progress_label);

    frame.render_widget(gauge, left_chunks[1]);
}

/// Render the right pane containing the peer connectivity table.
fn render_peers_table(state: &TuiState, frame: &mut Frame, area: Rect) {
    let title = format!(" Connected Peers ({}) ", state.peers.len());
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));

    if state.peers.is_empty() {
        let empty_msg = Paragraph::new("  No peers connected\n  Run `ferry pair` to link a device")
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty_msg, area);
        return;
    }

    let header = Row::new(vec!["Device ID", "Status", "Last Agreed", "Agreed At"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

    let rows: Vec<Row> = state
        .peers
        .iter()
        .map(|peer| {
            let short_dev = truncate_str(&peer.device_id, 12);
            let conn_style = match peer.connectivity.as_str() {
                "reachable" | "direct" => Style::default().fg(Color::Green),
                "unreachable" => Style::default().fg(Color::Red),
                "relay" => Style::default().fg(Color::Cyan),
                _ => Style::default().fg(Color::Yellow),
            };
            let agreed_manifest = peer
                .last_agreed_manifest_id
                .as_deref()
                .map_or("-", |m| truncate_str(m, 12));
            let agreed_at = peer.agreed_at.as_deref().unwrap_or("-");

            Row::new(vec![
                Span::styled(short_dev, Style::default().fg(Color::White)),
                Span::styled(&peer.connectivity, conn_style),
                Span::styled(agreed_manifest, Style::default().fg(Color::DarkGray)),
                Span::styled(agreed_at, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(30),
        Constraint::Percentage(24),
        Constraint::Percentage(24),
        Constraint::Percentage(22),
    ];

    let table = Table::new(rows, widths).header(header).block(block);
    frame.render_widget(table, area);
}

/// Render the bottom pane displaying recent activity log entries from the circular buffer.
fn render_activity_log(state: &TuiState, frame: &mut Frame, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Recent Activity ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    if inner_area.height == 0 {
        return;
    }

    let max_lines = inner_area.height as usize;
    let entries = state.activity_log.entries();
    let skip_count = entries.len().saturating_sub(max_lines);

    let items: Vec<ListItem> = entries
        .iter()
        .skip(skip_count)
        .map(|entry| {
            let (level_str, level_style) = match entry.level {
                LogLevel::Info => ("INFO", Style::default().fg(Color::White)),
                LogLevel::Warn => ("WARN", Style::default().fg(Color::Yellow)),
                LogLevel::Error => ("ERR ", Style::default().fg(Color::Red)),
                LogLevel::Success => (" OK ", Style::default().fg(Color::Green)),
            };

            let line = Line::from(vec![
                Span::styled(
                    format!("[{}] ", entry.timestamp),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("[{level_str}] "), level_style),
                Span::styled(&entry.message, Style::default().fg(Color::White)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner_area);
}

/// Render the single-line footer containing hotkey shortcuts.
fn render_footer(_state: &TuiState, frame: &mut Frame, area: Rect) {
    let hotkey_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::DarkGray);

    let spans = vec![
        Span::styled(" [A] ", hotkey_style),
        Span::styled("Add/Open  ", label_style),
        Span::styled("[P] ", hotkey_style),
        Span::styled("Pin  ", label_style),
        Span::styled("[R] ", hotkey_style),
        Span::styled("Rescan  ", label_style),
        Span::styled("[C] ", hotkey_style),
        Span::styled("Conflicts  ", label_style),
        Span::styled("[Q] ", hotkey_style),
        Span::styled("Quit", label_style),
    ];

    let footer = Paragraph::new(Line::from(spans));
    frame.render_widget(footer, area);
}

/// Render the interactive directory explorer modal.
fn render_folder_picker_modal(state: &TuiState, frame: &mut Frame, area: Rect) {
    let modal_area = centered_rect(80, 75, area);
    frame.render_widget(Clear, modal_area);

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Filesystem Explorer — Select Folder to Sync ",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    let inner_area = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    if inner_area.height < 4 {
        return;
    }

    // Split inner area into:
    // [0] Current directory path (1 line)
    // [1] Filter input field (1 line)
    // [2] Instruction / Shortcut hints (1 line)
    // [3] Entries list (remaining height)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner_area);

    // 1. Current path
    let cur_path_str = state.folder_picker.current_path.display().to_string();
    let path_line = Line::from(vec![
        Span::styled(
            " Directory: ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            cur_path_str,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(path_line), chunks[0]);

    // 2. Filter input
    let filter_text = if state.folder_picker.filter_query.is_empty() {
        Span::styled("(type to filter)", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(
            &state.folder_picker.filter_query,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )
    };
    let filter_line = Line::from(vec![
        Span::styled(
            " Filter:    ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        filter_text,
    ]);
    frame.render_widget(Paragraph::new(filter_line), chunks[1]);

    // 3. Navigation shortcuts
    let shortcuts_line = Line::from(vec![
        Span::styled(
            " [↑/↓] ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[Enter] ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Open Dir  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[Space] ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Select Folder  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "[Esc/Q] ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(shortcuts_line), chunks[2]);

    // 4. Entries list
    let items = state.folder_picker.filtered_items();
    let selected_idx = state.folder_picker.selected_index;

    if items.is_empty() {
        let empty_msg = if let Some(ref err) = state.folder_picker.error_message {
            format!("  Error: {err}")
        } else {
            "  (No matching folders or files found)".to_string()
        };
        let p = Paragraph::new(empty_msg).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, chunks[3]);
        return;
    }

    let max_visible = chunks[3].height as usize;
    let scroll_offset = if selected_idx >= max_visible {
        selected_idx - max_visible + 1
    } else {
        0
    };

    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(max_visible)
        .map(|(idx, item)| {
            let is_selected = idx == selected_idx;
            let cursor = if is_selected { "❯ " } else { "  " };

            match item {
                crate::state::FolderPickerItem::Parent(_) => {
                    let style = if is_selected {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    let line = Line::from(vec![
                        Span::styled(cursor, Style::default().fg(Color::Yellow)),
                        Span::styled("📁 .. (parent directory)", style),
                    ]);
                    ListItem::new(line)
                }
                crate::state::FolderPickerItem::Entry(entry) => {
                    let mut spans = Vec::new();
                    spans.push(Span::styled(cursor, Style::default().fg(Color::Yellow)));

                    let icon = if entry.is_dir { "📁 " } else { "📄 " };
                    spans.push(Span::raw(icon));

                    let name_style = if is_selected {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else if entry.is_dir {
                        Style::default().fg(Color::White)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    };
                    spans.push(Span::styled(&entry.name, name_style));

                    if entry.is_git_repo {
                        spans.push(Span::styled(
                            " [git]",
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ));
                    }
                    if entry.is_already_synced {
                        spans.push(Span::styled(
                            " [synced]",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ));
                    }
                    if entry.is_symlink {
                        spans.push(Span::styled(" [link]", Style::default().fg(Color::Magenta)));
                    }

                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();

    let list_widget = List::new(list_items);
    frame.render_widget(list_widget, chunks[3]);
}

/// Render the quarantined conflict inspector modal.
fn render_conflicts_modal(state: &TuiState, frame: &mut Frame, area: Rect) {
    let modal_area = centered_rect(75, 70, area);
    frame.render_widget(Clear, modal_area);

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Quarantined Conflicts ",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    let inner_area = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    if state.conflict_entries.is_empty() && state.conflicts == 0 {
        let content = Paragraph::new(
            "No quarantined conflict files detected in .ferry/conflicts.jsonl.\n\nPress [Esc], [Q], or [C] to return to dashboard.",
        )
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center);
        frame.render_widget(content, inner_area);
        return;
    }

    let mut lines = Vec::new();
    let total_count = if state.conflict_entries.is_empty() {
        state.conflicts
    } else {
        state.conflict_entries.len().max(state.conflicts)
    };

    lines.push(Line::from(Span::styled(
        format!("Total Conflicts: {total_count}"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    for entry in &state.conflict_entries {
        lines.push(Line::from(vec![
            Span::styled("Path: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(&entry.path, Style::default().fg(Color::Red)),
            Span::styled("  [", Style::default().fg(Color::DarkGray)),
            Span::styled(&entry.ts, Style::default().fg(Color::DarkGray)),
            Span::styled("]", Style::default().fg(Color::DarkGray)),
        ]));
        if let Some(ref q) = entry.quarantined_as {
            lines.push(Line::from(vec![
                Span::styled("  Quarantined: ", Style::default().fg(Color::DarkGray)),
                Span::styled(q, Style::default().fg(Color::Yellow)),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled("  Winner: ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate_str(&entry.winner.device, 16)),
            Span::styled("  Loser: ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate_str(&entry.loser.device, 16)),
        ]));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "Press [Esc], [Q], or [C] to close",
        Style::default().fg(Color::DarkGray),
    )));

    let list = Paragraph::new(lines);
    frame.render_widget(list, inner_area);
}

/// Helper function to center a rectangular area within another.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Helper to truncate strings safely without panicking.
#[must_use]
pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}
