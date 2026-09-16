//! Content-driven sidebar width (#895 follow-up, Opus/user review,
//! 2026-09-16).
//!
//! Two earlier attempts squeezed the sidebar's content (header title, the
//! Simple/Advanced selector, the nav tab list) into a WIDTH that was a fixed
//! function of `ui_text_scale` alone (`theme::sidebar_width`), eliding
//! labels that did not fit. That is backwards: `app.rs` now measures how
//! wide the content actually needs to be and sizes the panel to it, so
//! nothing is clipped or elided at a normal window size. The old
//! `sidebar_width` stays as the floor (never narrower than before) and the
//! fallback when nothing better is available.
//!
//! [`sidebar_content_width`] is the single entry point, called from
//! `app.rs` BEFORE the sidebar `Panel::left` is constructed. It measures
//! PURELY from font metrics via `ctx.fonts_mut(...)` — never from the
//! panel's own `available_width`, which does not exist yet at that point and
//! which, if read as an input to the value that DECIDES the panel's width,
//! would create a feedback loop that oscillates frame to frame.

use super::*;

/// Sidebar panel content-area horizontal inner margin, EACH side. Mirrors
/// `Panel::left("primary_navigation")`'s frame (`app.rs`:
/// `Margin::symmetric(14, 14)`).
pub(in crate::ui) const SIDEBAR_INNER_MARGIN: f32 = 14.0;

/// Header icon size (`tabs::shell::sidebar`), absolute px, not scaled.
pub(in crate::ui) const HEADER_ICON_SIZE: f32 = 25.0;
/// Header title size (`tabs::shell::sidebar`), absolute px, not scaled.
pub(in crate::ui) const HEADER_TITLE_SIZE: f32 = 18.0;
/// `TextStyle::Button` base size (`apply_ui_theme`, `theme.rs`): the font
/// the Simple/Advanced selector's buttons render with (no `.size()`
/// override on their `RichText`).
pub(in crate::ui) const MODE_BUTTON_FONT_SIZE: f32 = 14.0;
/// Nav-tab-row text size (`theme::nav_button`), absolute px, not scaled.
pub(in crate::ui) const NAV_BUTTON_TEXT_SIZE: f32 = 15.0;

/// Fraction of the WHOLE window's content width the sidebar may claim at
/// most, so a genuinely narrow window still leaves room for the central
/// content panel. Below this the header title's `.truncate()` and the
/// selector's stacked-rows fallback engage — the one case where eliding is
/// the right answer.
pub(in crate::ui) const MAX_WINDOW_WIDTH_FRACTION: f32 = 0.45;

/// The sidebar's content-driven width: at least enough to show the header
/// title, the wider of the two Simple/Advanced selector buttons side by
/// side, and the widest CURRENTLY VISIBLE nav tab label — all IN FULL, with
/// no eliding — never narrower than the legacy [`sidebar_width`] floor, and
/// never wider than [`MAX_WINDOW_WIDTH_FRACTION`] of `window_width`.
///
/// Pure given `(mode, raw_language, raw_scale, window_width)` for a fixed
/// `ctx` (egui's font metrics do not depend on anything this function
/// chooses), so calling it twice with identical inputs returns the
/// identical value — the oscillation guard `app.rs` needs, since this
/// value in turn decides how wide the panel that produces `available_width`
/// will be.
///
/// Measures only the tabs `tab_visible(mode, _)` currently allows and the
/// labels for `raw_language`, so the width is deterministic per (mode,
/// language, scale) and does not jitter frame to frame; it DOES change when
/// the user switches Simple ↔ Advanced or Simple/Advanced's tab set
/// changes, which is expected (the content that must fit changed).
pub(in crate::ui) fn sidebar_content_width(
    ctx: &egui::Context,
    mode: SettingsMode,
    raw_language: &str,
    raw_scale: &str,
    window_width: f32,
) -> f32 {
    let scale = layout_scale(raw_scale);
    // Mirrors `apply_ui_theme`'s `button_padding = vec2(10.0 * scale, ...)`.
    let button_padding_x = 10.0 * scale;
    let content = header_content_width(ctx, scale)
        .max(mode_selector_content_width(
            ctx,
            raw_language,
            scale,
            button_padding_x,
        ))
        .max(visible_tabs_content_width(
            ctx,
            mode,
            raw_language,
            button_padding_x,
        ));
    let wanted = content + 2.0 * SIDEBAR_INNER_MARGIN;
    let floor = sidebar_width(raw_scale);
    // `cap` must never fall below `floor`, or `.clamp` below would panic
    // (min > max) — a genuinely tiny window loses the 45% guideline rather
    // than the "never narrower than before" guarantee.
    let cap = (window_width * MAX_WINDOW_WIDTH_FRACTION).max(floor);
    wanted.clamp(floor, cap)
}

/// The width of `text` at `font_size` (absolute px, `FontFamily::Proportional`
/// — the family every widget here renders with, including the material-icon
/// codepoints, which `egui_material_icons::initialize` merges into that same
/// family), via `egui`'s OWN layout engine so this always matches what the
/// widget itself will paint.
pub(in crate::ui) fn text_width(ctx: &egui::Context, text: &str, font_size: f32) -> f32 {
    ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(font_size),
                egui::Color32::WHITE, // Color never affects layout metrics.
            )
            .size()
            .x
    })
}

/// Icon + item-spacing + title, exactly as laid out in
/// `tabs::shell::sidebar`'s header `ui.horizontal`.
pub(in crate::ui) fn header_content_width(ctx: &egui::Context, scale: f32) -> f32 {
    let icon_width = text_width(
        ctx,
        egui_material_icons::icons::ICON_KEYBOARD_VOICE.codepoint,
        HEADER_ICON_SIZE,
    );
    let item_spacing_x = ITEM_SPACING_X * scale;
    let title_width = text_width(ctx, "whisper-dictate", HEADER_TITLE_SIZE);
    icon_width + item_spacing_x + title_width
}

/// The full side-by-side pair's width: `settings_mode.rs`'s own
/// `mode_selector_layout` accounting (`2 * button_width + 2 * stroke`),
/// where `button_width` is the longer label's real width plus the REAL
/// (unshrunk) `button_padding.x`.
pub(in crate::ui) fn mode_selector_content_width(
    ctx: &egui::Context,
    raw_language: &str,
    scale: f32,
    button_padding_x: f32,
) -> f32 {
    let button_font_size = MODE_BUTTON_FONT_SIZE * scale;
    let longest_label = SettingsMode::ALL
        .into_iter()
        .map(|mode| text_width(ctx, mode.label(raw_language), button_font_size))
        .fold(0.0_f32, f32::max);
    let button_width = longest_label + 2.0 * button_padding_x;
    2.0 * button_width + 2.0 * MODE_BUTTON_STROKE
}

/// The widest nav row currently visible under `mode`: icon + two spaces +
/// label (exactly `theme::icon_text`'s format), at the nav button's own
/// fixed 15 px size, plus its button padding on both sides.
pub(in crate::ui) fn visible_tabs_content_width(
    ctx: &egui::Context,
    mode: SettingsMode,
    raw_language: &str,
    button_padding_x: f32,
) -> f32 {
    Tab::ALL
        .into_iter()
        .filter(|&tab| tab_visible(mode, tab))
        .map(|tab| {
            let combined = format!("{}  {}", tab.icon(), tab.label(raw_language));
            text_width(ctx, &combined, NAV_BUTTON_TEXT_SIZE) + 2.0 * button_padding_x
        })
        .fold(0.0_f32, f32::max)
}
