//! Pulsating status beacon widget for Ferry GUI.
//!
//! Renders a pulsating status dot with expanding aura waves rendered via egui's
//! painter, supporting Synced, Syncing, Holding/Pinned, Conflict, and Offline states.

use egui::{vec2, Color32, Painter, Pos2, Rounding, Stroke};

use crate::theme::colors;

/// Visual operational state for the status beacon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeaconState {
    Synced,
    Syncing,
    Holding,
    Conflict,
    Idle,
    Offline,
}

impl BeaconState {
    /// State primary color.
    #[must_use]
    pub const fn color(&self) -> Color32 {
        match self {
            Self::Synced => colors::FERRY_GREEN,
            Self::Syncing => colors::BLUE_SYNCING,
            Self::Holding => colors::PURPLE_PINNED,
            Self::Conflict => colors::RED_CONFLICT,
            Self::Idle => colors::CARD_BG,
            Self::Offline => colors::GRAY_OFFLINE,
        }
    }

    /// Label string for this status.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Synced => "SYNCED",
            Self::Syncing => "SYNCING",
            Self::Holding => "HOLDING",
            Self::Conflict => "CONFLICT",
            Self::Idle => "IDLE",
            Self::Offline => "OFFLINE",
        }
    }

    /// Pulse animation speed multiplier.
    #[must_use]
    pub const fn pulse_speed(&self) -> f64 {
        match self {
            Self::Synced => 0.8,   // Gentle breath
            Self::Syncing => 2.0,  // Active energetic waves
            Self::Holding => 1.0,  // Steady pulse
            Self::Conflict => 3.0, // Rapid warning pulse
            Self::Idle | Self::Offline => 0.0,
        }
    }
}

/// Render the pulsating beacon dot with animated aura waves.
pub fn render_pulsating_beacon(painter: &Painter, center: Pos2, state: BeaconState, time: f64) {
    let speed = state.pulse_speed();
    let base_color = state.color();

    // 1. Render expanding pulsating aura rings if active
    if speed > 0.0 {
        let phase1 = ((time * speed) % 1.0) as f32;
        let phase2 = (((time * speed) + 0.5) % 1.0) as f32;

        let max_expansion = match state {
            BeaconState::Syncing => 14.0f32,
            BeaconState::Conflict => 16.0f32,
            BeaconState::Holding => 10.0f32,
            _ => 8.0f32,
        };

        // Ring 1
        let r1 = 6.0f32 + phase1 * max_expansion;
        let factor1 = (1.0f32 - phase1).clamp(0.0, 1.0) * 0.45;
        let alpha1 = (factor1 * 255.0) as u8;
        let color1 = Color32::from_rgba_premultiplied(
            (f32::from(base_color.r()) * factor1) as u8,
            (f32::from(base_color.g()) * factor1) as u8,
            (f32::from(base_color.b()) * factor1) as u8,
            alpha1,
        );
        painter.circle_stroke(center, r1, Stroke::new(1.5f32, color1));

        // Ring 2 (offset)
        let r2 = 6.0f32 + phase2 * max_expansion;
        let factor2 = (1.0f32 - phase2).clamp(0.0, 1.0) * 0.30;
        let alpha2 = (factor2 * 255.0) as u8;
        let color2 = Color32::from_rgba_premultiplied(
            (f32::from(base_color.r()) * factor2) as u8,
            (f32::from(base_color.g()) * factor2) as u8,
            (f32::from(base_color.b()) * factor2) as u8,
            alpha2,
        );
        painter.circle_stroke(center, r2, Stroke::new(1.0f32, color2));
    }

    // 2. Render solid center core
    let core_radius = 5.0f32;
    painter.circle_filled(center, core_radius, base_color);
    painter.circle_stroke(
        center,
        core_radius,
        Stroke::new(1.0f32, Color32::from_white_alpha(120)),
    );
}

/// Render the complete Status Beacon widget including beacon aura and status badge/text.
pub fn status_beacon_ui(ui: &mut egui::Ui, state: BeaconState, time: f64) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
        let center = rect.center();
        render_pulsating_beacon(ui.painter(), center, state, time);

        // Status badge pill
        let bg = match state {
            BeaconState::Synced => colors::FERRY_GREEN,
            BeaconState::Syncing => colors::BLUE_SYNCING,
            BeaconState::Holding => colors::PURPLE_PINNED,
            BeaconState::Conflict => colors::RED_CONFLICT,
            BeaconState::Idle => colors::CARD_BG,
            BeaconState::Offline => colors::GRAY_OFFLINE,
        };
        let fg = match state {
            BeaconState::Synced => Color32::BLACK,
            BeaconState::Idle => colors::TEXT_PRIMARY,
            _ => Color32::WHITE,
        };

        let padding = vec2(8.0f32, 3.0f32);
        let font_id = egui::FontId::proportional(11.5f32);
        let galley = ui
            .painter()
            .layout_no_wrap(state.label().to_string(), font_id, fg);
        let (badge_rect, _) =
            ui.allocate_exact_size(galley.size() + padding * 2.0f32, egui::Sense::hover());

        ui.painter()
            .rect_filled(badge_rect, Rounding::same(8.0f32), bg);
        ui.painter().rect_stroke(
            badge_rect,
            Rounding::same(8.0f32),
            Stroke::new(1.0f32, colors::GLASS_BORDER_STRONG),
        );
        ui.painter().galley(badge_rect.min + padding, galley, fg);
    });

    if state.pulse_speed() > 0.0 {
        ui.ctx().request_repaint();
    }
}
