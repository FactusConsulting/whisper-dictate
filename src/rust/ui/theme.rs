//! Theme palette, colour/dimension constants, and the egui style/visuals setup
//! plus the shared chrome widgets (nav buttons, panel frames, sidebar bridge).

use super::*;
use std::collections::BTreeMap;

pub(in crate::ui) const DEFAULT_UI_TEXT_SCALE: f32 = 1.15;

const UI_BG: egui::Color32 = egui::Color32::from_rgb(13, 18, 24);
const UI_PANEL_BG: egui::Color32 = egui::Color32::from_rgb(18, 25, 33);
const UI_HEADER_BG: egui::Color32 = egui::Color32::from_rgb(16, 24, 32);
const UI_SURFACE_BG: egui::Color32 = egui::Color32::from_rgb(24, 34, 45);
const UI_SURFACE_HOVER_BG: egui::Color32 = egui::Color32::from_rgb(31, 45, 59);
const UI_SURFACE_ACTIVE_BG: egui::Color32 = egui::Color32::from_rgb(35, 56, 72);
const UI_BORDER: egui::Color32 = egui::Color32::from_rgb(48, 64, 79);
const UI_BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(38, 51, 65);
const UI_TEXT: egui::Color32 = egui::Color32::from_rgb(226, 235, 243);
const UI_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(143, 160, 176);
const UI_ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(125, 211, 252);
const UI_ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(14, 84, 112);
const UI_SELECTION_BG: egui::Color32 = egui::Color32::from_rgb(12, 92, 123);
const UI_OK_TEXT: egui::Color32 = egui::Color32::from_rgb(110, 231, 183);
const UI_WARN_TEXT: egui::Color32 = egui::Color32::from_rgb(251, 191, 36);
const UI_ERROR_TEXT: egui::Color32 = egui::Color32::from_rgb(251, 113, 133);
const UI_LIGHT_BG: egui::Color32 = egui::Color32::from_rgb(238, 244, 250);
const UI_LIGHT_PANEL_BG: egui::Color32 = egui::Color32::from_rgb(248, 251, 254);
const UI_LIGHT_HEADER_BG: egui::Color32 = egui::Color32::from_rgb(226, 239, 249);
const UI_LIGHT_SURFACE_BG: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
const UI_LIGHT_SURFACE_HOVER_BG: egui::Color32 = egui::Color32::from_rgb(230, 242, 252);
const UI_LIGHT_SURFACE_ACTIVE_BG: egui::Color32 = egui::Color32::from_rgb(204, 228, 246);
const UI_LIGHT_BORDER: egui::Color32 = egui::Color32::from_rgb(174, 194, 212);
const UI_LIGHT_BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(205, 219, 232);
const UI_LIGHT_TEXT: egui::Color32 = egui::Color32::from_rgb(28, 39, 52);
const UI_LIGHT_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(86, 102, 119);
const UI_LIGHT_ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(14, 116, 144);
const UI_LIGHT_ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(194, 225, 244);
const UI_LIGHT_SELECTION_BG: egui::Color32 = egui::Color32::from_rgb(191, 219, 254);
const UI_LIGHT_OK_TEXT: egui::Color32 = egui::Color32::from_rgb(21, 128, 61);
const UI_LIGHT_WARN_TEXT: egui::Color32 = egui::Color32::from_rgb(180, 83, 9);
const UI_LIGHT_ERROR_TEXT: egui::Color32 = egui::Color32::from_rgb(190, 18, 60);

const SIDEBAR_WIDTH: f32 = 164.0;
const TOP_STATUS_HEIGHT: f32 = 64.0;
pub(in crate::ui) const CONTROL_RADIUS: u8 = 8;
pub(in crate::ui) const PANEL_RADIUS: u8 = 12;
pub(in crate::ui) const PILL_RADIUS: u8 = 14;

/// Uniform inset between content and the surrounding panel/window edges. Used as
/// the CentralPanel margin and reused for the settings footer card so the gap is
/// identical on every edge instead of drifting between ad-hoc values.
pub(in crate::ui) const EDGE_MARGIN: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum UiThemeMode {
    Dark,
    Light,
}

impl UiThemeMode {
    fn from_raw(raw: &str) -> Self {
        match raw {
            "light" => Self::Light,
            _ => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::ui) struct UiPalette {
    pub(in crate::ui) bg: egui::Color32,
    pub(in crate::ui) panel_bg: egui::Color32,
    pub(in crate::ui) header_bg: egui::Color32,
    pub(in crate::ui) surface_bg: egui::Color32,
    pub(in crate::ui) surface_hover_bg: egui::Color32,
    pub(in crate::ui) surface_active_bg: egui::Color32,
    pub(in crate::ui) border: egui::Color32,
    pub(in crate::ui) border_soft: egui::Color32,
    pub(in crate::ui) text: egui::Color32,
    pub(in crate::ui) text_muted: egui::Color32,
    pub(in crate::ui) accent_blue: egui::Color32,
    pub(in crate::ui) accent_dark: egui::Color32,
    pub(in crate::ui) selection_bg: egui::Color32,
    pub(in crate::ui) ok_text: egui::Color32,
    pub(in crate::ui) warn_text: egui::Color32,
    pub(in crate::ui) error_text: egui::Color32,
}

pub(in crate::ui) fn ui_palette(raw_theme: &str) -> UiPalette {
    match UiThemeMode::from_raw(raw_theme) {
        UiThemeMode::Dark => UiPalette {
            bg: UI_BG,
            panel_bg: UI_PANEL_BG,
            header_bg: UI_HEADER_BG,
            surface_bg: UI_SURFACE_BG,
            surface_hover_bg: UI_SURFACE_HOVER_BG,
            surface_active_bg: UI_SURFACE_ACTIVE_BG,
            border: UI_BORDER,
            border_soft: UI_BORDER_SOFT,
            text: UI_TEXT,
            text_muted: UI_TEXT_MUTED,
            accent_blue: UI_ACCENT_BLUE,
            accent_dark: UI_ACCENT_DARK,
            selection_bg: UI_SELECTION_BG,
            ok_text: UI_OK_TEXT,
            warn_text: UI_WARN_TEXT,
            error_text: UI_ERROR_TEXT,
        },
        UiThemeMode::Light => UiPalette {
            bg: UI_LIGHT_BG,
            panel_bg: UI_LIGHT_PANEL_BG,
            header_bg: UI_LIGHT_HEADER_BG,
            surface_bg: UI_LIGHT_SURFACE_BG,
            surface_hover_bg: UI_LIGHT_SURFACE_HOVER_BG,
            surface_active_bg: UI_LIGHT_SURFACE_ACTIVE_BG,
            border: UI_LIGHT_BORDER,
            border_soft: UI_LIGHT_BORDER_SOFT,
            text: UI_LIGHT_TEXT,
            text_muted: UI_LIGHT_TEXT_MUTED,
            accent_blue: UI_LIGHT_ACCENT_BLUE,
            accent_dark: UI_LIGHT_ACCENT_DARK,
            selection_bg: UI_LIGHT_SELECTION_BG,
            ok_text: UI_LIGHT_OK_TEXT,
            warn_text: UI_LIGHT_WARN_TEXT,
            error_text: UI_LIGHT_ERROR_TEXT,
        },
    }
}

pub(in crate::ui) fn apply_ui_theme(ctx: &egui::Context, raw_scale: &str, raw_theme: &str) {
    let theme = UiThemeMode::from_raw(raw_theme);
    let palette = ui_palette(raw_theme);
    let scale = raw_scale
        .trim()
        .parse::<f32>()
        .unwrap_or(DEFAULT_UI_TEXT_SCALE)
        .clamp(0.85, 1.6);
    // Single source of truth for font sizes/weights. Headers go through
    // `ui.heading()` / `section_label` (Heading / Small), body text and labels
    // through Body, code/paths through Monospace, buttons through Button. Avoid
    // ad-hoc `RichText::size()` for headers/labels — add or reuse a style here.
    let text_styles = BTreeMap::from([
        (
            egui::TextStyle::Heading,
            egui::FontId::proportional(18.0 * scale),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::proportional(14.0 * scale),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::monospace(13.0 * scale),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::proportional(14.0 * scale),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::proportional(12.0 * scale),
        ),
    ]);
    let button_padding = egui::vec2(10.0 * scale, 5.0 * scale);
    let item_spacing = egui::vec2(9.0 * scale, 7.0 * scale);
    let mut style = (*ctx.style()).clone();
    style.text_styles = text_styles;
    style.spacing.button_padding = button_padding;
    style.spacing.item_spacing = item_spacing;
    style.spacing.interact_size = egui::vec2(42.0 * scale, 28.0 * scale);
    style.visuals = themed_visuals(theme, palette);
    ctx.set_style(style);
}

fn themed_visuals(theme: UiThemeMode, palette: UiPalette) -> egui::Visuals {
    let mut visuals = match theme {
        UiThemeMode::Dark => egui::Visuals::dark(),
        UiThemeMode::Light => egui::Visuals::light(),
    };
    visuals.override_text_color = Some(palette.text);
    visuals.panel_fill = palette.panel_bg;
    visuals.window_fill = palette.panel_bg;
    visuals.faint_bg_color = palette.surface_bg;
    visuals.extreme_bg_color = palette.bg;
    visuals.code_bg_color = palette.bg;
    visuals.hyperlink_color = palette.accent_blue;
    visuals.warn_fg_color = palette.warn_text;
    visuals.error_fg_color = palette.error_text;
    visuals.selection.bg_fill = palette.selection_bg;
    visuals.selection.stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.noninteractive.bg_fill = palette.panel_bg;
    visuals.widgets.noninteractive.weak_bg_fill = palette.panel_bg;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.8, palette.border_soft);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette.text_muted);
    visuals.widgets.inactive.bg_fill = palette.surface_bg;
    visuals.widgets.inactive.weak_bg_fill = palette.surface_bg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(0.8, palette.border_soft);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.hovered.bg_fill = palette.surface_hover_bg;
    visuals.widgets.hovered.weak_bg_fill = palette.surface_hover_bg;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.active.bg_fill = palette.surface_active_bg;
    visuals.widgets.active.weak_bg_fill = palette.surface_active_bg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.open.bg_fill = palette.accent_dark;
    visuals.widgets.open.weak_bg_fill = palette.accent_dark;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, palette.accent_blue);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, palette.text);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.inactive.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.hovered.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.active.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.widgets.open.rounding = egui::Rounding::same(CONTROL_RADIUS as f32);
    visuals.window_rounding = egui::Rounding::same(PANEL_RADIUS as f32);
    visuals
}

pub(in crate::ui) fn nav_button(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    label: &str,
    palette: UiPalette,
) -> egui::Response {
    let fill = if selected {
        palette.accent_dark
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke = if selected {
        egui::Stroke::new(1.0, palette.accent_blue)
    } else {
        egui::Stroke::NONE
    };
    let text = if selected {
        icon_text(icon, label)
            .size(15.0)
            .strong()
            .color(palette.text)
    } else {
        icon_text(icon, label).size(15.0).color(palette.text_muted)
    };
    ui.add_sized(
        egui::vec2(ui.available_width(), 38.0),
        egui::Button::new(text).fill(fill).stroke(stroke),
    )
}

pub(in crate::ui) fn icon_text(icon: &str, label: impl AsRef<str>) -> egui::RichText {
    egui::RichText::new(format!("{icon}  {}", label.as_ref()))
}

/// Parse a user scale string into the clamped layout multiplier (default 1.0).
/// Trims surrounding whitespace to match `apply_ui_theme`'s scale parsing.
fn layout_scale(raw_scale: &str) -> f32 {
    raw_scale
        .trim()
        .parse::<f32>()
        .unwrap_or(1.0)
        .clamp(0.85, 1.6)
}

pub(in crate::ui) fn sidebar_width(raw_scale: &str) -> f32 {
    SIDEBAR_WIDTH * layout_scale(raw_scale)
}

pub(in crate::ui) fn paint_sidebar_bridge(
    ctx: &egui::Context,
    palette: UiPalette,
    raw_scale: &str,
) {
    let screen = ctx.screen_rect();
    let left = screen.left() + sidebar_width(raw_scale) - 1.0;
    let bridge = egui::Rect::from_min_max(
        egui::pos2(left, screen.top()),
        egui::pos2((left + 16.0).min(screen.right()), screen.bottom()),
    );
    ctx.layer_painter(egui::LayerId::background())
        .rect_filled(bridge, 0.0, palette.panel_bg);
}

pub(in crate::ui) fn top_status_bar_height(raw_scale: &str) -> f32 {
    TOP_STATUS_HEIGHT * layout_scale(raw_scale)
}

pub(in crate::ui) fn panel_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
}

pub(in crate::ui) fn inset_panel_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_palette_selects_dark_by_default_and_light_only_for_light() {
        // Unknown / empty theme strings fall back to the dark palette.
        assert_eq!(ui_palette("dark").bg, UI_BG);
        assert_eq!(ui_palette("").bg, UI_BG);
        assert_eq!(ui_palette("nonsense").bg, UI_BG);
        // Only the explicit "light" value swaps to the light palette.
        assert_eq!(ui_palette("light").bg, UI_LIGHT_BG);
        assert_ne!(ui_palette("light").bg, ui_palette("dark").bg);
        assert_eq!(ui_palette("light").text, UI_LIGHT_TEXT);
    }

    #[test]
    fn apply_ui_theme_sets_scaled_body_font_and_clamps_extremes() {
        let ctx = egui::Context::default();
        apply_ui_theme(&ctx, "1.0", "dark");
        let body = ctx.style().text_styles[&egui::TextStyle::Body].size;
        assert!((body - 14.0).abs() < 0.001);

        // Out-of-range scales are clamped (max 1.6) before being applied.
        apply_ui_theme(&ctx, "99", "light");
        let clamped = ctx.style().text_styles[&egui::TextStyle::Body].size;
        assert!((clamped - 14.0 * 1.6).abs() < 0.001);
    }

    #[test]
    fn layout_scale_trims_clamps_and_defaults() {
        // Surrounding whitespace is trimmed before parsing (matches apply_ui_theme).
        assert!((layout_scale(" 1.2 ") - 1.2).abs() < 0.001);
        // Unparseable input falls back to 1.0.
        assert!((layout_scale("nonsense") - 1.0).abs() < 0.001);
        // Out-of-range values are clamped to [0.85, 1.6].
        assert!((layout_scale("99") - 1.6).abs() < 0.001);
        assert!((layout_scale("0.1") - 0.85).abs() < 0.001);
    }
}
