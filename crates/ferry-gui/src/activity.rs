use egui::{Align, Color32, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Stroke};

use crate::theme::colors;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub timestamp: String,
    pub category: &'static str,
    pub message: String,
    pub color: Color32,
}

impl ActivityEntry {
    #[must_use]
    pub fn new(category: &'static str, message: impl Into<String>, color: Color32) -> Self {
        let ts = ferry_platform::time::current_time_str();
        let short_ts = if ts.len() > 19 {
            ts[11..19].to_string()
        } else {
            ts
        };
        Self {
            timestamp: short_ts,
            category,
            message: message.into(),
            color,
        }
    }
}

pub fn render_activity_stream(
    ui: &mut egui::Ui,
    activity_log: &[ActivityEntry],
    auto_scroll: &mut bool,
    mut on_clear: impl FnMut(),
) {
    Frame::none()
        .fill(colors::CARD_BG)
        .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
        .rounding(Rounding::same(10.0f32))
        .inner_margin(Margin::same(14.0f32))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Real-Time Activity Stream ({})",
                        activity_log.len()
                    ))
                    .strong()
                    .color(colors::TEXT_PRIMARY)
                    .size(13.5),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(RichText::new("Clear").size(11.5)).clicked() {
                        on_clear();
                    }
                    ui.checkbox(auto_scroll, RichText::new("Auto-scroll").size(11.5));
                });
            });

            ui.add_space(8.0);

            Frame::none()
                .fill(colors::PANEL_BG)
                .stroke(Stroke::new(1.0f32, colors::GLASS_BORDER))
                .rounding(Rounding::same(6.0f32))
                .inner_margin(Margin::same(8.0f32))
                .show(ui, |ui| {
                    let mut scroll = ScrollArea::vertical()
                        .max_height(160.0f32)
                        .min_scrolled_height(80.0f32)
                        .auto_shrink([false, false]);

                    if *auto_scroll {
                        scroll = scroll.stick_to_bottom(true);
                    }

                    scroll.show(ui, |ui| {
                        if activity_log.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No activity recorded yet. Waiting for sync events...",
                                )
                                .color(colors::TEXT_MUTED)
                                .size(12.0),
                            );
                        } else {
                            for entry in activity_log {
                                ui.horizontal(|ui| {
                                    ui.monospace(
                                        RichText::new(&entry.timestamp)
                                            .color(colors::TEXT_MUTED)
                                            .size(11.0),
                                    );

                                    let cat_text = format!("[{}]", entry.category);
                                    ui.monospace(
                                        RichText::new(cat_text)
                                            .color(entry.color)
                                            .strong()
                                            .size(11.0),
                                    );

                                    ui.label(
                                        RichText::new(&entry.message)
                                            .color(colors::TEXT_PRIMARY)
                                            .size(12.0),
                                    );
                                });
                            }
                        }
                    });
                });
        });
}
