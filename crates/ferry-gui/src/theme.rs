//! Obsidian dark theme engine and fluid glass visual tokens for Ferry GUI.

use egui::{epaint::Shadow, vec2, Color32, FontId, Margin, Rounding, Stroke, TextStyle, Visuals};

/// Color palette constants for the Ferry Obsidian design system.
pub mod colors {
    use egui::Color32;

    pub const OBSIDIAN_BG: Color32 = Color32::from_rgb(0x09, 0x09, 0x0b);
    pub const PANEL_BG: Color32 = Color32::from_rgba_premultiplied(18, 18, 24, 190);
    pub const CARD_BG: Color32 = Color32::from_rgba_premultiplied(24, 24, 32, 210);
    pub const HOVER_BG: Color32 = Color32::from_rgba_premultiplied(36, 36, 48, 220);
    pub const ACTIVE_BG: Color32 = Color32::from_rgba_premultiplied(48, 48, 64, 240);

    pub const GLASS_BORDER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 20);
    pub const GLASS_BORDER_STRONG: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 38);

    pub const FERRY_GREEN: Color32 = Color32::from_rgb(0x2e, 0xcc, 0x71);
    pub const AMBER_WARN: Color32 = Color32::from_rgb(0xf3, 0x9c, 0x12);
    pub const RED_CONFLICT: Color32 = Color32::from_rgb(0xe7, 0x4c, 0x3c);
    pub const BLUE_SYNCING: Color32 = Color32::from_rgb(0x34, 0x98, 0xdb);
    pub const PURPLE_PINNED: Color32 = Color32::from_rgb(0x9b, 0x59, 0xb6);
    pub const GRAY_MUTED: Color32 = Color32::from_rgb(0x71, 0x71, 0x7a);
    pub const GRAY_OFFLINE: Color32 = Color32::from_rgb(0x52, 0x52, 0x5b);

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf5);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xaa);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x71, 0x71, 0x7a);
}

/// Helper for building themed obsidian visuals and styles in `egui`.
pub struct Theme;

impl Theme {
    /// Apply the Obsidian Dark design theme to an `egui::Context`.
    pub fn apply(ctx: &egui::Context) {
        let mut visuals = Visuals::dark();

        visuals.override_text_color = Some(colors::TEXT_PRIMARY);
        visuals.dark_mode = true;
        visuals.panel_fill = colors::OBSIDIAN_BG;
        visuals.window_fill = colors::CARD_BG;
        visuals.window_stroke = Stroke::new(1.0f32, colors::GLASS_BORDER_STRONG);
        visuals.window_shadow = Shadow {
            offset: vec2(0.0f32, 8.0f32),
            blur: 24.0f32,
            spread: 0.0f32,
            color: Color32::from_black_alpha(160),
        };
        visuals.window_rounding = Rounding::same(12.0f32);

        // Non-interactive widgets
        visuals.widgets.noninteractive.bg_fill = colors::PANEL_BG;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0f32, colors::GLASS_BORDER);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0f32, colors::TEXT_PRIMARY);
        visuals.widgets.noninteractive.rounding = Rounding::same(8.0f32);

        // Inactive widgets (buttons, text inputs)
        visuals.widgets.inactive.bg_fill = colors::CARD_BG;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0f32, colors::GLASS_BORDER);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0f32, colors::TEXT_PRIMARY);
        visuals.widgets.inactive.rounding = Rounding::same(8.0f32);

        // Hovered widgets
        visuals.widgets.hovered.bg_fill = colors::HOVER_BG;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0f32, colors::FERRY_GREEN);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0f32, Color32::WHITE);
        visuals.widgets.hovered.rounding = Rounding::same(8.0f32);

        // Active / pressed widgets
        visuals.widgets.active.bg_fill = colors::ACTIVE_BG;
        visuals.widgets.active.bg_stroke = Stroke::new(1.5f32, colors::FERRY_GREEN);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0f32, Color32::WHITE);
        visuals.widgets.active.rounding = Rounding::same(8.0f32);

        // Selection
        visuals.selection.bg_fill = Color32::from_rgba_premultiplied(46, 204, 113, 60);
        visuals.selection.stroke = Stroke::new(1.0f32, colors::FERRY_GREEN);

        ctx.set_visuals(visuals);

        // Typography scale
        let mut style = (*ctx.style()).clone();
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(20.0f32));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0f32));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0f32));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.5f32));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5f32));
        style.spacing.item_spacing = vec2(8.0f32, 8.0f32);
        style.spacing.window_margin = Margin::same(16.0f32);
        ctx.set_style(style);
    }

    /// Render a pill-shaped status badge with background and text color.
    pub fn render_status_badge(
        ui: &mut egui::Ui,
        text: &str,
        bg_color: Color32,
        text_color: Color32,
    ) {
        let padding = vec2(10.0f32, 4.0f32);
        let font_id = FontId::proportional(12.0f32);
        let galley = ui
            .painter()
            .layout_no_wrap(text.to_string(), font_id, text_color);
        let (rect, _response) =
            ui.allocate_exact_size(galley.size() + padding * 2.0f32, egui::Sense::hover());

        ui.painter()
            .rect_filled(rect, Rounding::same(10.0f32), bg_color);
        ui.painter().rect_stroke(
            rect,
            Rounding::same(10.0f32),
            Stroke::new(1.0f32, colors::GLASS_BORDER_STRONG),
        );
        ui.painter().galley(rect.min + padding, galley, text_color);
    }
}
