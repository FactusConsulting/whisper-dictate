//! Theme palette, colour/dimension constants, and the egui style/visuals setup
//! plus the shared chrome widgets (nav buttons, panel frames, sidebar bridge).

use super::*;
use std::collections::BTreeMap;

pub(in crate::ui) const DEFAULT_UI_TEXT_SCALE: f32 = 1.15;

const UI_BG: egui::Color32 = egui::Color32::from_rgb(13, 18, 24);
const UI_PANEL_BG: egui::Color32 = egui::Color32::from_rgb(18, 25, 33);
const UI_HEADER_BG: egui::Color32 = egui::Color32::from_rgb(16, 24, 32);
// Dedicated fill for the top-bar readout cards (Status / Backend / Model /
// Post pill). Slightly darker than UI_PANEL_BG (18,25,33) so the cards read as
// a subtle recess instead of blending flush with the panel background. Must NOT
// be the same value as UI_HEADER_BG because the header is the top panel's own
// fill; keeping them distinct lets the readouts and the surrounding bar have
// visually separate surfaces. Aim: perceptible but flat — not a raised button.
const UI_READOUT_BG: egui::Color32 = egui::Color32::from_rgb(12, 18, 25);
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
// Base (scale-1.0) text sizes used by the two-line status card. These mirror the
// `apply_ui_theme` text styles (Small = 12, Body = 14) and the vertical
// item-spacing (7) so the panel-height math below tracks the real rendered card
// instead of a hand-tuned magic number. (The legacy fixed `TOP_STATUS_HEIGHT = 64`
// constant is gone — the panel height is now derived from the real card content;
// see `top_status_bar_height`.)
const SMALL_FONT_SIZE: f32 = 12.0;
const BODY_FONT_SIZE: f32 = 14.0;
pub(in crate::ui) const ITEM_SPACING_Y: f32 = 7.0;
// Vertical inner margins (UNSCALED literals, like the horizontal ones): the top
// panel's frame margin (`app.rs`) and each card/pill's frame margin. The panel
// height is derived from these so the rounded card bottom is never clipped.
pub(in crate::ui) const TOP_PANEL_V_MARGIN: f32 = 10.0;
pub(in crate::ui) const STATUS_CARD_V_MARGIN: f32 = 9.0;
// Optical-centering correction for the two-line status card.
//
// Diagnosis (egui 0.30 font metrics): the first-line galley's `mesh_bounds`
// start ~2-4px BELOW the galley rect top (a fixed per-line leading offset
// that egui adds to the ink position but not the layout rect), while the last
// line's ink is flush with the galley bottom.  This creates a persistent
// asymmetry: with symmetric V_MARGIN, the VISIBLE air above the text is always
// 2-4px more than the air below.
//
// Fix: reduce the top inner margin by this constant so the content block
// shifts upward by REDUCTION px, bringing the visible ink optically centred.
// REDUCTION=2 (unscaled, like the other margin constants) perfectly balances
// the gap at scales 0.85-1.0 (imbalance 2px → 0px) and halves the residual
// at scales 1.15-1.6 (imbalance 4px → 2px).  Total card height =
// (V_MARGIN - REDUCTION) + content + V_MARGIN which is 2px less than the
// previous symmetric layout — panel-height headroom absorbs the difference.
pub(in crate::ui) const STATUS_CARD_V_TOP_REDUCTION: f32 = 2.0;
// A little extra breathing room below the card so its rounded corners sit clear
// of the panel's bottom edge at every scale. Exposed to the ui module so the
// layout tests can reference it instead of a raw literal.
pub(in crate::ui) const TOP_STATUS_V_HEADROOM: f32 = 4.0;
// Rough width of the post-indicator pill (icon + "Post on/off" + margins)
// at scale 1.0.
const POST_INDICATOR_MIN_WIDTH: f32 = 120.0;
// Minimum width for a regular status card (Status / Backend / Task) at scale 1.0.
const STATUS_CARD_MIN_WIDTH: f32 = 134.0;
// Minimum width for the wide stt-detail card (Model/Compute) at scale 1.0.
const STATUS_CARD_WIDE_MIN_WIDTH: f32 = 218.0;
// Frame geometry shared between the card/pill RENDER and the top-bar fit budget
// so the two can never drift. `set_min_width` sizes the inner CONTENT; the card's
// real OUTER width is `inner*scale + 2*H_MARGIN + 2*CARD_STROKE`. The margin and
// stroke are UNSCALED literals (they come straight from the Frame builder), so
// they are added AFTER the scale multiply. These MUST match the literals in
// `status_card_sized`'s Frame (margin 14) and `post_indicator`'s Frame
// (margin 12) — both reference these consts. The status cards/pill are flat
// READOUTS (no border) so people don't mistake them for the Start/Stop buttons,
// hence `CARD_STROKE = 0.0`; it stays in the outer-width budget so the geometry
// source-of-truth is preserved if a border is ever reintroduced.
pub(in crate::ui) const STATUS_CARD_H_MARGIN: f32 = 14.0;
pub(in crate::ui) const POST_PILL_H_MARGIN: f32 = 12.0;
pub(in crate::ui) const CARD_STROKE: f32 = 0.0;
// Base horizontal item-spacing (scaled by the UI text scale in `apply_ui_theme`).
// The top-bar fit budget separates cards by exactly this scaled value, so the
// layout tests source it here instead of re-hardcoding the literal.
pub(in crate::ui) const ITEM_SPACING_X: f32 = 9.0;
const BOTTOM_MESSAGE_BAR_HEIGHT: f32 = 30.0;
pub(in crate::ui) const CONTROL_RADIUS: u8 = 8;
pub(in crate::ui) const PANEL_RADIUS: u8 = 12;
pub(in crate::ui) const PILL_RADIUS: u8 = 14;
// Top-bar status READOUTS (cards + post pill). Deliberately a tighter, nearly
// flat corner than the buttons' `CONTROL_RADIUS` (8) so the readouts don't read
// as the raised, clickable Start/Stop/compact buttons next to them.
pub(in crate::ui) const READOUT_RADIUS: u8 = 4;

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
    /// Fill for the top-bar status readout cards and post-indicator pill.
    /// Kept separate from `header_bg` so the two surfaces can be tuned
    /// independently — in dark mode this is slightly darker than `panel_bg`
    /// (recessed feel); in light mode it mirrors `header_bg` which is already
    /// distinctly lighter than the panel.
    pub(in crate::ui) readout_bg: egui::Color32,
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
            readout_bg: UI_READOUT_BG,
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
            // Light-mode readouts use the same header_bg — it's already
            // distinctly lighter than the panel so cards have natural shape.
            readout_bg: UI_LIGHT_HEADER_BG,
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

/// Parse a raw scale string into a clamped `f32` multiplier.
///
/// Single source of truth shared by `apply_ui_theme` (text rendering) and
/// `layout_scale` (panel/card sizing). Both paths MUST return the same value
/// for any input so text and layout always agree — the clipping bug this PR
/// fixed was caused by the two paths using different fallbacks for garbage
/// input (1.0 vs DEFAULT_UI_TEXT_SCALE).
fn parse_ui_scale(raw: &str) -> f32 {
    raw.trim()
        .parse::<f32>()
        .unwrap_or(DEFAULT_UI_TEXT_SCALE)
        .clamp(0.85, 1.6)
}

pub(in crate::ui) fn apply_ui_theme(ctx: &egui::Context, raw_scale: &str, raw_theme: &str) {
    let theme = UiThemeMode::from_raw(raw_theme);
    let palette = ui_palette(raw_theme);
    let scale = parse_ui_scale(raw_scale);
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
            egui::FontId::proportional(BODY_FONT_SIZE * scale),
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
            egui::FontId::proportional(SMALL_FONT_SIZE * scale),
        ),
    ]);
    let button_padding = egui::vec2(10.0 * scale, 5.0 * scale);
    let item_spacing = egui::vec2(ITEM_SPACING_X * scale, ITEM_SPACING_Y * scale);
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
    // Inactive tabs get a subtle visible surface + soft border so they read as
    // clickable affordances rather than bare text; the selected tab keeps the
    // stronger accent fill + accent-blue outline. Hover highlight + a pointing
    // hand cursor reinforce that every row is a button.
    let fill = if selected {
        palette.accent_dark
    } else {
        palette.surface_bg
    };
    let stroke = if selected {
        egui::Stroke::new(1.0, palette.accent_blue)
    } else {
        egui::Stroke::new(0.8, palette.border_soft)
    };
    let text = if selected {
        icon_text(icon, label)
            .size(15.0)
            .strong()
            .color(palette.text)
    } else {
        icon_text(icon, label).size(15.0).color(palette.text_muted)
    };
    let response = ui.add_sized(
        egui::vec2(ui.available_width(), 38.0),
        egui::Button::new(text).fill(fill).stroke(stroke),
    );
    if selected {
        response
    } else {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

pub(in crate::ui) fn icon_text(icon: &str, label: impl AsRef<str>) -> egui::RichText {
    egui::RichText::new(format!("{icon}  {}", label.as_ref()))
}

/// Parse a user scale string into the clamped layout multiplier.
/// Delegates to `parse_ui_scale` so text rendering (`apply_ui_theme`) and
/// panel/card sizing always use the same value — including the same fallback
/// (`DEFAULT_UI_TEXT_SCALE`) on garbage input.
fn layout_scale(raw_scale: &str) -> f32 {
    parse_ui_scale(raw_scale)
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

/// Height of a two-line status card's rendered content + its own frame margin,
/// at the given UI scale. The card stacks a Small label, the vertical
/// item-spacing, and a Body value (all scaled), wrapped in the card's asymmetric
/// vertical inner margin: `(V_MARGIN - TOP_REDUCTION)` at top and `V_MARGIN` at
/// bottom (unscaled), where the top reduction optically centres the ink inside
/// the card fill (see `STATUS_CARD_V_TOP_REDUCTION`). Pure so the panel-height
/// fit is unit-testable without an egui context.
pub(in crate::ui) fn status_card_height(raw_scale: &str) -> f32 {
    let scale = layout_scale(raw_scale);
    let text = (SMALL_FONT_SIZE + ITEM_SPACING_Y + BODY_FONT_SIZE) * scale;
    // Asymmetric margins: (V_MARGIN - REDUCTION) at top + V_MARGIN at bottom.
    text + (STATUS_CARD_V_MARGIN - STATUS_CARD_V_TOP_REDUCTION) + STATUS_CARD_V_MARGIN
}

/// Exact height of the top status panel. Derived from the actual two-line card
/// height plus the panel's own vertical frame margin and a little headroom, so
/// the card's rounded bottom is fully visible (never clipped) at every scale —
/// the unscaled card/panel margins no longer fall behind the scaled text.
pub(in crate::ui) fn top_status_bar_height(raw_scale: &str) -> f32 {
    status_card_height(raw_scale) + 2.0 * TOP_PANEL_V_MARGIN + TOP_STATUS_V_HEADROOM
}

/// Minimum remaining width before the top-bar post pill is drawn at all.
/// The pill's rendered size grows with the UI text scale, so the threshold
/// must scale with it (Copilot finding on PR #170).
pub(in crate::ui) fn post_indicator_min_width(raw_scale: &str) -> f32 {
    POST_INDICATOR_MIN_WIDTH * layout_scale(raw_scale)
}

/// Minimum width for a regular (narrow) status card — Status, Backend, Task.
/// Scales with the UI text scale so the budget check uses the same number the
/// card's `set_min_width` call actually requests.
pub(in crate::ui) fn status_card_min_width(raw_scale: &str) -> f32 {
    STATUS_CARD_MIN_WIDTH * layout_scale(raw_scale)
}

/// Minimum width for the wide stt-detail card (Model / Compute).
/// Scales with the UI text scale.
pub(in crate::ui) fn status_card_wide_min_width(raw_scale: &str) -> f32 {
    STATUS_CARD_WIDE_MIN_WIDTH * layout_scale(raw_scale)
}

/// The TRUE outer width a regular status card occupies in the bar: the scaled
/// inner content min-width plus the Frame's symmetric horizontal inner margin
/// (`2*STATUS_CARD_H_MARGIN`, unscaled) and both stroke edges (`2*CARD_STROKE`,
/// unscaled). The top-bar fit budget MUST use this — `set_min_width` only sizes
/// the inner content, so feeding it the bare inner width undercounts each card
/// by the margin+stroke and the cards overflow and clip mid-card.
pub(in crate::ui) fn status_card_outer_width(raw_scale: &str) -> f32 {
    status_card_min_width(raw_scale) + 2.0 * STATUS_CARD_H_MARGIN + 2.0 * CARD_STROKE
}

/// Outer width of the wide stt-detail card (same margin/stroke as the narrow
/// card; only the inner min-width differs).
pub(in crate::ui) fn status_card_wide_outer_width(raw_scale: &str) -> f32 {
    status_card_wide_min_width(raw_scale) + 2.0 * STATUS_CARD_H_MARGIN + 2.0 * CARD_STROKE
}

/// Outer width of the post-indicator pill. The pill's Frame uses a tighter
/// horizontal margin (`POST_PILL_H_MARGIN`) than the cards, but the same stroke.
pub(in crate::ui) fn post_indicator_outer_width(raw_scale: &str) -> f32 {
    post_indicator_min_width(raw_scale) + 2.0 * POST_PILL_H_MARGIN + 2.0 * CARD_STROKE
}

pub(in crate::ui) fn bottom_message_bar_height(raw_scale: &str) -> f32 {
    BOTTOM_MESSAGE_BAR_HEIGHT * layout_scale(raw_scale)
}

pub(in crate::ui) fn panel_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
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
        // Unparseable input now falls back to DEFAULT_UI_TEXT_SCALE (same as
        // apply_ui_theme) so text rendering and panel sizing agree on every input.
        assert!(
            (layout_scale("nonsense") - DEFAULT_UI_TEXT_SCALE).abs() < 0.001,
            "layout_scale fallback must equal DEFAULT_UI_TEXT_SCALE"
        );
        // Out-of-range values are clamped to [0.85, 1.6].
        assert!((layout_scale("99") - 1.6).abs() < 0.001);
        assert!((layout_scale("0.1") - 0.85).abs() < 0.001);
    }

    #[test]
    fn parse_ui_scale_and_apply_ui_theme_agree_on_fallback() {
        // The clipping bug this PR fixed was caused by layout_scale and
        // apply_ui_theme using DIFFERENT fallbacks for garbage input (1.0 vs
        // DEFAULT_UI_TEXT_SCALE). Verify both paths now return the same value
        // for garbage, valid, and clamped inputs so text and layout never drift.
        let ctx = egui::Context::default();

        // Garbage input: parse_ui_scale (= layout_scale) must return exactly
        // DEFAULT_UI_TEXT_SCALE, and apply_ui_theme must set Body to that scale.
        let fallback_scale = parse_ui_scale("garbage");
        assert!(
            (fallback_scale - DEFAULT_UI_TEXT_SCALE).abs() < 0.001,
            "parse_ui_scale fallback {fallback_scale} != DEFAULT_UI_TEXT_SCALE {DEFAULT_UI_TEXT_SCALE}"
        );
        apply_ui_theme(&ctx, "garbage", "dark");
        let body_on_garbage = ctx.style().text_styles[&egui::TextStyle::Body].size;
        assert!(
            (body_on_garbage - BODY_FONT_SIZE * DEFAULT_UI_TEXT_SCALE).abs() < 0.001,
            "apply_ui_theme set body size {body_on_garbage} but expected {}",
            BODY_FONT_SIZE * DEFAULT_UI_TEXT_SCALE
        );
        assert!(
            (body_on_garbage - BODY_FONT_SIZE * fallback_scale).abs() < 0.001,
            "text scale {body_on_garbage} and layout scale {} disagree on garbage input",
            BODY_FONT_SIZE * fallback_scale
        );

        // Valid input and clamped input: both paths must agree.
        for raw in ["1.0", "1.15", " 1.3 ", "0.1", "99"] {
            let layout = parse_ui_scale(raw);
            apply_ui_theme(&ctx, raw, "dark");
            let text = ctx.style().text_styles[&egui::TextStyle::Body].size / BODY_FONT_SIZE;
            assert!(
                (text - layout).abs() < 0.001,
                "text scale {text} and layout scale {layout} disagree for input {raw:?}"
            );
        }
    }
}
