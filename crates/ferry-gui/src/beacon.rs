use egui::{vec2, Color32, Painter, Pos2, Rounding, Stroke};

use crate::theme::colors;
use ferry_platform::SyncState;

pub fn beacon_color(state: SyncState) -> Color32 {
    match state {
        SyncState::Synced => colors::FERRY_GREEN,
        SyncState::Syncing => colors::BLUE_SYNCING,
        SyncState::Pinned => colors::PURPLE_PINNED,
        SyncState::Conflict => colors::RED_CONFLICT,
        SyncState::Idle => colors::CARD_BG,
        SyncState::Offline => colors::GRAY_OFFLINE,
        SyncState::Error => colors::RED_CONFLICT,
    }
}

pub fn beacon_label(state: SyncState) -> &'static str {
    state.label()
}

pub fn beacon_pulse_speed(state: SyncState) -> f64 {
    state.pulse_speed()
}

pub fn render_pulsating_beacon(painter: &Painter, center: Pos2, state: SyncState, time: f64) {
    let speed = state.pulse_speed();
    let base_color = beacon_color(state);

    if speed > 0.0 {
        let phase1 = ((time * speed) % 1.0) as f32;
        let phase2 = (((time * speed) + 0.5) % 1.0) as f32;

        let max_expansion = match state {
            SyncState::Syncing => 14.0f32,
            SyncState::Conflict => 16.0f32,
            SyncState::Pinned => 10.0f32,
            _ => 8.0f32,
        };

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

    let core_radius = 5.0f32;
    painter.circle_filled(center, core_radius, base_color);
    painter.circle_stroke(
        center,
        core_radius,
        Stroke::new(1.0f32, Color32::from_white_alpha(120)),
    );
}

pub fn status_beacon_ui(ui: &mut egui::Ui, state: SyncState, time: f64) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
        let center = rect.center();
        render_pulsating_beacon(ui.painter(), center, state, time);

        let bg = beacon_color(state);
        let fg = match state {
            SyncState::Synced => Color32::BLACK,
            SyncState::Idle => colors::TEXT_PRIMARY,
            _ => Color32::WHITE,
        };

        let padding = vec2(8.0f32, 3.0f32);
        let font_id = egui::FontId::proportional(11.5f32);
        let galley = ui
            .painter()
            .layout_no_wrap(beacon_label(state).to_string(), font_id, fg);
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

    if beacon_pulse_speed(state) > 0.0 {
        ui.ctx().request_repaint();
    }
}
