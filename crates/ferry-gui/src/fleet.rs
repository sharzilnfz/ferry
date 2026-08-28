//! Connected device fleet table widget for Ferry GUI.
//!
//! Displays paired peer device rows with device IDs, agreement timestamps,
//! and connectivity status pills (Online, Dialing, Offline).

use egui::{vec2, Align, Color32, Frame, Layout, Margin, RichText, Rounding, Stroke};
use ferry_ipc::protocol::PeerStatusView;

use crate::telemetry::format_short_hex;
use crate::theme::colors;

/// Render the Connected Device Fleet table.
pub fn render_fleet_table(
    ui: &mut egui::Ui,
    peers: &[PeerStatusView],
    mut on_pair_click: impl FnMut(),
    mut on_share_click: impl FnMut(),
) {
    Frame::none()
        .fill(colors::CARD_BG)
        .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
        .rounding(Rounding::same(10.0f32))
        .inner_margin(Margin::same(14.0f32))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Connected Device Fleet ({})", peers.len()))
                        .strong()
                        .color(colors::TEXT_PRIMARY)
                        .size(13.5),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(RichText::new("+ Pair Device").size(12.0).color(colors::FERRY_GREEN)).clicked() {
                        on_pair_click();
                    }
                    if ui.button(RichText::new("Share Folder").size(12.0)).clicked() {
                        on_share_click();
                    }
                });
            });

            ui.add_space(8.0);

            if peers.is_empty() {
                Frame::none()
                    .fill(colors::PANEL_BG)
                    .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                    .rounding(Rounding::same(6.0f32))
                    .inner_margin(Margin::same(16.0f32))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("No peer devices connected.")
                                    .color(colors::TEXT_MUTED)
                                    .size(13.0),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("Click '+ Pair Device' to accept a pairing offer, or 'Share Folder' to generate a pairing token.")
                                    .color(colors::TEXT_SECONDARY)
                                    .size(11.5),
                            );
                        });
                    });
            } else {
                for (idx, peer) in peers.iter().enumerate() {
                    render_peer_row(ui, peer, idx);
                    if idx + 1 < peers.len() {
                        ui.add_space(4.0);
                    }
                }
            }
        });
}

fn render_peer_row(ui: &mut egui::Ui, peer: &PeerStatusView, _idx: usize) {
    Frame::none()
        .fill(colors::PANEL_BG)
        .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
        .rounding(Rounding::same(6.0f32))
        .inner_margin(Margin::symmetric(12.0f32, 8.0f32))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Connectivity status pill
                let conn_lower = peer.connectivity.to_lowercase();
                let (status_text, bg, fg) =
                    if conn_lower.contains("online") || conn_lower.contains("connected") {
                        ("Online", colors::FERRY_GREEN, Color32::BLACK)
                    } else if conn_lower.contains("dial") || conn_lower.contains("connecting") {
                        ("Dialing", colors::AMBER_WARN, Color32::BLACK)
                    } else {
                        ("Offline", colors::GRAY_OFFLINE, colors::TEXT_MUTED)
                    };

                let padding = vec2(6.0f32, 2.0f32);
                let font_id = egui::FontId::proportional(11.0f32);
                let galley = ui
                    .painter()
                    .layout_no_wrap(status_text.to_string(), font_id, fg);
                let (badge_rect, _) =
                    ui.allocate_exact_size(galley.size() + padding * 2.0f32, egui::Sense::hover());
                ui.painter()
                    .rect_filled(badge_rect, Rounding::same(6.0f32), bg);
                ui.painter().galley(badge_rect.min + padding, galley, fg);

                ui.add_space(8.0);

                // Device ID
                let short_id = format_short_hex(Some(&peer.device_id));
                let dev_label = ui.monospace(
                    RichText::new(&short_id)
                        .color(colors::TEXT_PRIMARY)
                        .strong()
                        .size(12.5),
                );
                dev_label.on_hover_text(format!("Full Device ID:\n{}", peer.device_id));

                if ui
                    .small_button("📋")
                    .on_hover_text("Copy Device ID")
                    .clicked()
                {
                    ui.ctx()
                        .output_mut(|o| o.copied_text.clone_from(&peer.device_id));
                }

                // Agreed Manifest / Timestamp
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(ref agreed_at) = peer.agreed_at {
                        ui.label(
                            RichText::new(agreed_at)
                                .color(colors::TEXT_MUTED)
                                .size(11.5),
                        );
                        ui.label(
                            RichText::new("Agreed:")
                                .color(colors::TEXT_SECONDARY)
                                .size(11.0),
                        );
                    } else if let Some(ref last_manifest) = peer.last_agreed_manifest_id {
                        ui.monospace(
                            RichText::new(format!(
                                "manifest: {}",
                                format_short_hex(Some(last_manifest))
                            ))
                            .color(colors::GRAY_MUTED)
                            .size(11.0),
                        );
                    } else {
                        ui.label(
                            RichText::new("Pending initial sync")
                                .color(colors::TEXT_MUTED)
                                .size(11.0),
                        );
                    }
                });
            });
        });
}
