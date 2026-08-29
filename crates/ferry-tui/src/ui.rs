//! Ratatui UI layout and widget rendering for the Ferry TUI dashboard.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Row, Table},
    Frame,
};

use crate::activity_log::LogLevel;
use crate::picker::PickerState;
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
        Span::styled(" [P] ", hotkey_style),
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

/// Render the filesystem picker modal as a centered overlay.
pub fn render_picker(picker: &PickerState, frame: &mut Frame, area: Rect) {
    let modal_area = centered_rect(75, 70, area);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Select Folder ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(
                " Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" open  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Space",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" close", Style::default().fg(Color::DarkGray)),
        ]));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Length(1), // filter
            Constraint::Min(3),    // list
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Breadcrumb bar
    let breadcrumb = Paragraph::new(Line::from(vec![
        Span::styled(" Path: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(picker.breadcrumbs(), Style::default().fg(Color::White)),
    ]))
    .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(breadcrumb, chunks[0]);

    // Filter line
    let filter_text = if picker.filter.is_empty() {
        Span::styled(
            " Filter: (type to filter)",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::styled(
            format!(" Filter: {}", picker.filter),
            Style::default().fg(Color::Yellow),
        )
    };
    let filter_para = Paragraph::new(Line::from(vec![filter_text]));
    frame.render_widget(filter_para, chunks[1]);

    // Build list items: .. parent row + visible entries
    let mut items: Vec<ListItem> = Vec::new();

    // .. parent row (always visible when not filtered, dimmed if at root)
    let show_parent = picker.filter.is_empty();
    if show_parent {
        let parent_label = if picker.current_path.parent().is_some() {
            "📁  .. (parent)"
        } else {
            "—  .. (root)"
        };
        let parent_style = Style::default().fg(Color::DarkGray);
        items.push(ListItem::new(Line::from(vec![Span::styled(
            parent_label,
            parent_style,
        )])));
    }

    let visible = picker.visible_entries();
    for (idx, entry) in visible.iter().enumerate() {
        // Adjust index for cursor offset when parent row is shown
        let _ = idx;
        let icon = if entry.is_symlink {
            "🔗"
        } else if entry.is_dir {
            "📁"
        } else {
            "📄"
        };
        let mut spans = vec![
            Span::raw(format!("{icon}  ")),
            Span::styled(entry.name.clone(), Style::default().fg(Color::White)),
        ];
        if entry.is_git_repo {
            spans.push(Span::styled("  [git]", Style::default().fg(Color::Green)));
        }
        if entry.is_already_synced {
            spans.push(Span::styled(
                "  (synced)",
                Style::default().fg(Color::DarkGray),
            ));
        }
        let mut line = Line::from(spans);
        if entry.is_already_synced {
            line = line.style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            );
        }
        // Highlight cursor: need to compute effective cursor index matching this row's position
        let effective_cursor = picker.cursor;
        let row_idx = if show_parent { idx + 1 } else { idx };
        let is_selected = row_idx == effective_cursor;
        let mut item = ListItem::new(line);
        if is_selected {
            item = item.style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
        } else if entry.is_already_synced {
            item = item.style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            );
        }
        items.push(item);
    }

    if picker.loading {
        let loading = Paragraph::new(" Loading…")
            .style(Style::default().fg(Color::Yellow))
            .alignment(Alignment::Center);
        frame.render_widget(loading, chunks[2]);
    } else if items.is_empty() {
        let empty = Paragraph::new(" (no entries)")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        frame.render_widget(empty, chunks[2]);
    } else {
        let list = List::new(items)
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ ");
        frame.render_widget(list, chunks[2]);
    }

    // Hint / is_git_repo badge line
    let hint_line = if let Some(ref h) = picker.hint {
        Paragraph::new(Line::from(vec![Span::styled(
            h.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]))
    } else if picker.loading {
        Paragraph::new(Line::from(vec![Span::styled(
            "loading…",
            Style::default().fg(Color::DarkGray),
        )]))
    } else {
        Paragraph::new(Line::from(vec![Span::styled(
            "Space on folder to select • already-synced dirs dimmed",
            Style::default().fg(Color::DarkGray),
        )]))
    };
    frame.render_widget(hint_line, chunks[3]);
}
