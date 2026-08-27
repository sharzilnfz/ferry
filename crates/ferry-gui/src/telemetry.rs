//! Hairline telemetry strip widget for Ferry GUI.
//!
//! Displays key system metadata:
//! - Root Manifest ID (short hex)
//! - Held Changes count
//! - Active Conflict count
//! - Encryption Cipher (Age-X25519)
//! - Transport (QUIC / Iroh)

use egui::{Color32, Frame, Margin, RichText, Rounding, Stroke};
use ferry_ipc::protocol::EngineSnapshot;

use crate::theme::colors;

/// Format a manifest ID or hash to short hex display (e.g. `e3b0c442…`).
#[must_use]
pub fn format_short_hex(hash: Option<&str>) -> String {
    match hash {
        None | Some("") => "none".to_string(),
        Some(s) if s.len() > 12 => format!("{}…{}", &s[..6], &s[s.len() - 4..]),
        Some(s) => s.to_string(),
    }
}

/// Render the hairline telemetry strip widget.
pub fn render_telemetry_hairline(
    ui: &mut egui::Ui,
    snapshot: Option<&EngineSnapshot>,
    conflicts_count: usize,
    mut on_conflicts_click: impl FnMut(),
) {
    Frame::none()
        .fill(colors::CARD_BG)
        .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
        .inner_margin(Margin::symmetric(14.0f32, 6.0f32))
        .rounding(Rounding::same(6.0f32))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // 1. Root Manifest ID
                ui.label(RichText::new("ROOT:").color(colors::TEXT_MUTED).size(11.0).strong());
                let manifest_hex = snapshot.and_then(|s| s.manifest_id.as_deref());
                ui.monospace(
                    RichText::new(format_short_hex(manifest_hex))
                        .color(if manifest_hex.is_some() { colors::FERRY_GREEN } else { colors::GRAY_MUTED })
                        .size(11.5),
                );

                render_divider(ui);

                // 2. Held Changes
                let held = snapshot.map_or(0, |s| s.held_changes);
                ui.label(RichText::new("HELD:").color(colors::TEXT_MUTED).size(11.0).strong());
                let held_color = if held > 0 { colors::PURPLE_PINNED } else { colors::TEXT_SECONDARY };
                ui.monospace(RichText::new(format!("{held}")).color(held_color).size(11.5).strong());

                render_divider(ui);

                // 3. Conflicts Count
                let conf_count = snapshot.map_or(conflicts_count, |s| s.conflicts.max(conflicts_count));
                ui.label(RichText::new("CONFLICTS:").color(colors::TEXT_MUTED).size(11.0).strong());
                if conf_count > 0 {
                    if ui.button(
                        RichText::new(format!("{conf_count} ACTIVE"))
                            .color(Color32::WHITE)
                            .size(11.0)
                            .strong(),
                    ).clicked() {
                        on_conflicts_click();
                    }
                } else {
                    ui.monospace(RichText::new("0").color(colors::FERRY_GREEN).size(11.5));
                }

                render_divider(ui);

                // 4. Cipher
                ui.label(RichText::new("CIPHER:").color(colors::TEXT_MUTED).size(11.0).strong());
                ui.monospace(RichText::new("Age-X25519").color(colors::TEXT_SECONDARY).size(11.5));

                render_divider(ui);

                // 5. Transport
                ui.label(RichText::new("TRANSPORT:").color(colors::TEXT_MUTED).size(11.0).strong());
                ui.monospace(RichText::new("QUIC / Iroh").color(colors::BLUE_SYNCING).size(11.5));
            });
        });
}

fn render_divider(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.label(RichText::new("│").color(colors::GLASS_BORDER_STRONG).size(11.0));
    ui.add_space(6.0);
}
