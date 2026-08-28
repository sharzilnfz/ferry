//! Modal dialogs and inspection drawers for Ferry GUI.
//!
//! Includes:
//! - Quarantined Conflicts Inspection Drawer
//! - Share Folder Pairing Workflow (Offer Generation, Secret Warning & QR Code)
//! - Pair Device Acceptance Workflow
//! - Session Pinning Modal

use std::path::PathBuf;

use egui::{
    epaint::Shadow, vec2, Align, Align2, Area, Color32, Frame, Layout, Margin, Order, RichText,
    Rounding, ScrollArea, Stroke,
};
use ferry_ipc::backend::ShareOffer;
use ferry_ipc::protocol::ConflictEntry;
use qrcode::QrCode;

use crate::telemetry::format_short_hex;
use crate::theme::colors;

/// Render ASCII QR representation from a payload token.
#[must_use]
pub fn generate_ascii_qr(payload: &str) -> String {
    if let Ok(code) = QrCode::new(payload.as_bytes()) {
        code.render::<char>()
            .quiet_zone(false)
            .module_dimensions(2, 1)
            .build()
    } else {
        format!("[QR: {payload}]")
    }
}

/// Generic modal container with fluid-glass dark styling.
pub fn render_modal_frame(
    ctx: &egui::Context,
    title: &str,
    is_open: &mut bool,
    max_width: f32,
    content: impl FnOnce(&mut egui::Ui, &mut bool),
) {
    if !*is_open {
        return;
    }
    let mut open = true;
    Area::new(egui::Id::new(title))
        .anchor(Align2::CENTER_CENTER, vec2(0.0f32, 0.0f32))
        .order(Order::Foreground)
        .show(ctx, |ui| {
            Frame::none()
                .fill(colors::CARD_BG)
                .stroke(Stroke::new(1.5f32, colors::GLASS_BORDER_STRONG))
                .rounding(Rounding::same(12.0f32))
                .shadow(Shadow {
                    offset: vec2(0.0f32, 10.0f32),
                    blur: 30.0f32,
                    spread: 0.0f32,
                    color: Color32::from_black_alpha(220),
                })
                .inner_margin(Margin::same(20.0f32))
                .show(ui, |ui| {
                    ui.set_max_width(max_width);
                    ui.horizontal(|ui| {
                        ui.heading(RichText::new(title).strong().color(colors::TEXT_PRIMARY));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(RichText::new("✕").size(14.0)).clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.separator();
                    ui.add_space(8.0);
                    content(ui, &mut open);
                });
        });
    *is_open = open;
}

/// Quarantined Conflicts Inspection Drawer / Modal.
pub fn render_conflicts_modal(
    ctx: &egui::Context,
    is_open: &mut bool,
    conflicts: &[ConflictEntry],
    mut on_refresh: impl FnMut(),
) {
    render_modal_frame(
        ctx,
        "Quarantined Conflicts Inspection",
        is_open,
        560.0,
        |ui, _open| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Total Unresolved Conflicts: {}", conflicts.len()))
                        .color(if conflicts.is_empty() {
                            colors::FERRY_GREEN
                        } else {
                            colors::RED_CONFLICT
                        })
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("↻ Refresh").clicked() {
                        on_refresh();
                    }
                });
            });

            ui.add_space(8.0);

            if conflicts.is_empty() {
                Frame::none()
                    .fill(colors::PANEL_BG)
                    .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                    .rounding(Rounding::same(8.0f32))
                    .inner_margin(Margin::same(16.0f32))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("✓ No Quarantined Conflicts")
                                    .color(colors::FERRY_GREEN)
                                    .strong()
                                    .size(14.0),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "All local and remote file states are clean and reconciled.",
                                )
                                .color(colors::TEXT_MUTED)
                                .size(12.0),
                            );
                        });
                    });
            } else {
                ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    for (idx, c) in conflicts.iter().enumerate() {
                        Frame::none()
                            .fill(colors::PANEL_BG)
                            .stroke(Stroke::new(1.0f32, colors::RED_CONFLICT))
                            .rounding(Rounding::same(8.0f32))
                            .inner_margin(Margin::same(12.0f32))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("#{idx} • {}", c.ts))
                                            .color(colors::TEXT_MUTED)
                                            .size(11.0),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.monospace(
                                            RichText::new(format!("kind: {}", c.kind))
                                                .color(colors::AMBER_WARN)
                                                .size(11.0),
                                        );
                                    });
                                });

                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(&c.path)
                                        .color(colors::RED_CONFLICT)
                                        .strong()
                                        .size(13.0),
                                );

                                if let Some(ref q) = c.quarantined_as {
                                    ui.add_space(2.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("Quarantined copy:")
                                                .color(colors::TEXT_MUTED)
                                                .size(11.5),
                                        );
                                        ui.monospace(
                                            RichText::new(q)
                                                .color(colors::TEXT_SECONDARY)
                                                .size(11.5),
                                        );
                                    });
                                }

                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Winning device:")
                                            .color(colors::TEXT_MUTED)
                                            .size(11.5),
                                    );
                                    ui.monospace(
                                        RichText::new(format_short_hex(Some(&c.winner.device)))
                                            .color(colors::FERRY_GREEN)
                                            .size(11.5),
                                    );
                                    ui.label(RichText::new("│").color(colors::GLASS_BORDER));
                                    ui.label(
                                        RichText::new("Losing device:")
                                            .color(colors::TEXT_MUTED)
                                            .size(11.5),
                                    );
                                    ui.monospace(
                                        RichText::new(format_short_hex(Some(&c.loser.device)))
                                            .color(colors::RED_CONFLICT)
                                            .size(11.5),
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            }
        },
    );
}

/// Share Folder / Pairing Offer Modal Workflow.
pub fn render_share_modal(
    ctx: &egui::Context,
    is_open: &mut bool,
    active_offer: Option<&ShareOffer>,
    secret_warnings: &[String],
    override_secrets: &mut bool,
    mut on_generate_offer: impl FnMut(bool),
) {
    render_modal_frame(
        ctx,
        "Share Folder & Pairing Ritual",
        is_open,
        520.0,
        |ui, open| {
            if let Some(offer) = active_offer {
                // Success: Offer generated
                ui.label(RichText::new("Pairing offer is active! Scan QR code or copy the pairing token to the recipient device:").color(colors::TEXT_SECONDARY).size(12.5));
                ui.add_space(8.0);

                // Token display box
                Frame::none()
                    .fill(colors::PANEL_BG)
                    .stroke(Stroke::new(1.0f32, colors::FERRY_GREEN))
                    .rounding(Rounding::same(8.0f32))
                    .inner_margin(Margin::same(12.0f32))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("32-Character Pairing Short Code:")
                                .color(colors::TEXT_MUTED)
                                .size(11.0),
                        );
                        ui.horizontal(|ui| {
                            ui.monospace(
                                RichText::new(&offer.token)
                                    .color(colors::FERRY_GREEN)
                                    .strong()
                                    .size(13.0),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .button(RichText::new("📋 Copy Token").size(12.0))
                                    .clicked()
                                {
                                    ui.ctx()
                                        .output_mut(|o| o.copied_text.clone_from(&offer.token));
                                }
                            });
                        });

                        if let Some(ref path) = offer.payload_path {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Payload file:")
                                        .color(colors::TEXT_MUTED)
                                        .size(11.0),
                                );
                                ui.monospace(
                                    RichText::new(path.display().to_string())
                                        .color(colors::TEXT_SECONDARY)
                                        .size(11.0),
                                );
                            });
                        }
                    });

                ui.add_space(8.0);

                // QR Code representation
                let qr_string = generate_ascii_qr(&offer.token);
                Frame::none()
                    .fill(colors::OBSIDIAN_BG)
                    .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                    .rounding(Rounding::same(6.0f32))
                    .inner_margin(Margin::same(10.0f32))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("QR Code Representation")
                                    .color(colors::TEXT_MUTED)
                                    .size(11.0),
                            );
                            ui.add_space(4.0);
                            ScrollArea::both().max_height(140.0).show(ui, |ui| {
                                ui.monospace(
                                    RichText::new(qr_string).color(Color32::WHITE).size(8.0),
                                );
                            });
                        });
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Done").color(colors::FERRY_GREEN))
                        .clicked()
                    {
                        *open = false;
                    }
                });
            } else {
                // Initial Offer Generation Form
                ui.label(RichText::new("Generate an encrypted pairing offer to connect another device to this folder.").color(colors::TEXT_SECONDARY).size(12.5));
                ui.add_space(8.0);

                // Secret Scanning Warning Banner
                if !secret_warnings.is_empty() {
                    Frame::none()
                    .fill(Color32::from_rgba_premultiplied(243, 156, 18, 30))
                    .stroke(Stroke::new(1.5f32, colors::AMBER_WARN))
                    .rounding(Rounding::same(8.0f32))
                    .inner_margin(Margin::same(12.0f32))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("⚠️ Security Warning: {} Unignored Secret(s) Detected", secret_warnings.len()))
                                .color(colors::AMBER_WARN)
                                .strong()
                                .size(13.0),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Sharing this folder will grant the peer access to the following sensitive files:")
                                .color(colors::TEXT_SECONDARY)
                                .size(11.5),
                        );
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(80.0).show(ui, |ui| {
                            for w in secret_warnings {
                                ui.monospace(RichText::new(format!("• {w}")).color(colors::AMBER_WARN).size(11.0));
                            }
                        });
                        ui.add_space(6.0);
                        ui.checkbox(
                            override_secrets,
                            RichText::new("I understand the risks of sharing unignored secrets (Override)").strong().size(12.0),
                        );
                    });
                    ui.add_space(8.0);
                }

                ui.horizontal(|ui| {
                    let can_generate = secret_warnings.is_empty() || *override_secrets;
                    let btn = egui::Button::new(
                        RichText::new("Generate Pairing Offer")
                            .color(if can_generate {
                                Color32::BLACK
                            } else {
                                colors::TEXT_MUTED
                            })
                            .strong(),
                    )
                    .fill(if can_generate {
                        colors::FERRY_GREEN
                    } else {
                        colors::PANEL_BG
                    });

                    if ui.add_enabled(can_generate, btn).clicked() {
                        on_generate_offer(*override_secrets);
                    }

                    if ui.button("Cancel").clicked() {
                        *open = false;
                    }
                });
            }
        },
    );
}

/// Pair Device Acceptance Modal Workflow.
pub fn render_pair_modal(
    ctx: &egui::Context,
    is_open: &mut bool,
    pair_input: &mut String,
    mut on_accept_pair: impl FnMut(PathBuf),
) {
    render_modal_frame(
        ctx,
        "Pair Device (Accept Offer)",
        is_open,
        480.0,
        |ui, open| {
            ui.label(
                RichText::new(
                    "Enter the filesystem path to the incoming `.ferry-pair` offer payload file:",
                )
                .color(colors::TEXT_SECONDARY)
                .size(12.5),
            );
            ui.add_space(6.0);

            ui.text_edit_singleline(pair_input);
            ui.label(
                RichText::new("e.g. /home/user/downloads/pair-offer.ferry-pair")
                    .color(colors::TEXT_MUTED)
                    .size(11.0),
            );

            ui.add_space(12.0);

            ui.horizontal(|ui| {
                let is_valid = !pair_input.trim().is_empty();
                let btn = egui::Button::new(
                    RichText::new("Accept Pairing Offer")
                        .color(if is_valid {
                            Color32::BLACK
                        } else {
                            colors::TEXT_MUTED
                        })
                        .strong(),
                )
                .fill(if is_valid {
                    colors::FERRY_GREEN
                } else {
                    colors::PANEL_BG
                });

                if ui.add_enabled(is_valid, btn).clicked() {
                    let path = PathBuf::from(pair_input.trim());
                    on_accept_pair(path);
                    *open = false;
                }

                if ui.button("Cancel").clicked() {
                    *open = false;
                }
            });
        },
    );
}

/// Session Pinning Modal.
pub fn render_pin_modal(
    ctx: &egui::Context,
    is_open: &mut bool,
    paths_input: &mut String,
    mut on_start_pin: impl FnMut(Vec<String>, Option<u64>),
) {
    render_modal_frame(
        ctx,
        "Session Pinning (Hold Edits)",
        is_open,
        480.0,
        |ui, open| {
            ui.label(RichText::new("Declare this device the exclusive writer for selected paths. Remote edits will be held until pin is released:").color(colors::TEXT_SECONDARY).size(12.5));
            ui.add_space(6.0);

            ui.text_edit_singleline(paths_input);
            ui.label(RichText::new("Comma-separated glob paths, e.g.: src/**, tests/** (leave blank for entire folder)").color(colors::TEXT_MUTED).size(11.0));

            ui.add_space(12.0);

            ui.horizontal(|ui| {
                if ui
                    .button(
                        RichText::new("Start Session Pin (8h)")
                            .color(Color32::BLACK)
                            .strong(),
                    )
                    .clicked()
                {
                    let paths: Vec<String> = paths_input
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    on_start_pin(paths, Some(8));
                    *open = false;
                }

                if ui.button("Cancel").clicked() {
                    *open = false;
                }
            });
        },
    );
}
