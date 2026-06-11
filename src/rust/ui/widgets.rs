//! Reusable settings-grid form widgets: labelled text/combo/checkbox/password
//! rows, the inline help-badge machinery, and small layout helpers shared by the
//! settings tabs.  ComboBox helpers live in the sibling `widgets_combo` module
//! (declared in `ui.rs`) and are re-exported from here so all existing call
//! sites that import `super::*` are unaffected.

use super::*;
use std::time::{Duration, Instant};

// Re-export combo helpers from the sibling module so the glob `use super::*`
// in every settings tab keeps resolving them without any change at call sites.
pub(in crate::ui) use super::widgets_combo::*;

/// Minimum width for the label column in every settings grid at scale 1.0.
/// Use `settings_label_width(ui)` for the scaled value at render time.
pub(in crate::ui) const SETTINGS_LABEL_WIDTH: f32 = 220.0;
const SETTINGS_CONTROL_MAX_WIDTH: f32 = 420.0;
/// Fixed narrow width for combos whose options are SHORT enum tokens
/// (e.g. auto/type/paste, Off/Basic/Verbose, auto/cuda/cpu). Sized to comfortably
/// fit the longest such option plus the dropdown arrow without stretching the
/// whole grid the way long descriptive option labels (model pickers, compute
/// type) legitimately need. Scales with the UI text-scale via
/// `settings_short_control_width`.
const SETTINGS_SHORT_CONTROL_WIDTH: f32 = 240.0;
/// Compact width for short numeric-ish fields (counts, seconds, thresholds) so a
/// value like "2000" or "0.5" no longer stretches across the whole grid.
const SETTINGS_SHORT_INPUT_WIDTH: f32 = 120.0;
/// Width for path / longer free-text fields — narrower than the old full-width
/// stretch but still roomy enough to read a file path at a glance.
const SETTINGS_TEXT_INPUT_WIDTH: f32 = 360.0;

/// Pure scale-multiply for the label column width. Extracted so it can be
/// unit-tested without an egui context.
///
/// `body_size` is the current `Body` font size in points (e.g. `14.0 * scale`).
/// Returns `SETTINGS_LABEL_WIDTH * (body_size / 14.0)` so the column grows
/// proportionally with the UI text-scale setting.
pub(in crate::ui) fn scaled_label_width(body_size: f32) -> f32 {
    SETTINGS_LABEL_WIDTH * (body_size / 14.0)
}

/// Returns the label-column min-width appropriate for the current UI text scale.
/// Reads the live `Body` font size from the egui style so the column grows when
/// the user raises `ui_text_scale`, preventing long labels from wrapping.
///
/// This is the single alignment anchor for every settings grid: the label cell's
/// `set_min_width` pins column 0 across all grids (no grid-wide floor needed).
pub(in crate::ui) fn settings_label_width(ui: &egui::Ui) -> f32 {
    let body_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .map(|f| f.size)
        .unwrap_or(14.0);
    scaled_label_width(body_size)
}

pub(in crate::ui) fn text_help(ui: &mut egui::Ui, label: &str, value: &mut String, help: &str) {
    text_help_width(ui, label, value, help, SETTINGS_TEXT_INPUT_WIDTH);
}

/// Short-input variant for numeric/threshold fields. Same row/help machinery,
/// just a compact fixed input width.
pub(in crate::ui) fn text_help_short(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    help: &str,
) {
    text_help_width(ui, label, value, help, SETTINGS_SHORT_INPUT_WIDTH);
}

/// Clamp a raw numeric settings string into `[min, max]`, returning the
/// canonical string the rest of the settings pipeline stores.
///
/// Pure (no egui) so it is unit-testable. Behaviour:
/// - in-range value → re-formatted canonically (unchanged numerically),
/// - above `max` → `max`, below `min` → `min`,
/// - unparseable garbage → clamp the schema `default` into range; if the
///   default is itself unparseable, fall back to `min`,
/// - integer fields format without a decimal point; float fields are
///   canonicalized (trailing zeros trimmed, e.g. `0.30` → `0.3`).
pub(in crate::ui) fn clamp_numeric_setting(
    raw: &str,
    min: f64,
    max: f64,
    is_int: bool,
    default: &str,
) -> String {
    let parsed = raw
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .or_else(|| default.trim().parse::<f64>().ok().filter(|v| v.is_finite()));
    let value = match parsed {
        Some(v) => v.clamp(min, max),
        None => min,
    };
    format_numeric(value, is_int)
}

/// Format a clamped numeric value back to its canonical string. Integers drop
/// the fractional part; floats trim trailing zeros so `0.30` shows as `0.3`.
fn format_numeric(value: f64, is_int: bool) -> String {
    if is_int {
        return (value.round() as i64).to_string();
    }
    // Format with enough precision, then trim trailing zeros / dot so a value
    // like 0.3 doesn't become "0.300000".
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    if s == "-0" {
        s = "0".to_owned();
    }
    s
}

fn text_help_width(ui: &mut egui::Ui, label: &str, value: &mut String, help: &str, width: f32) {
    let show_help = label_with_help(ui, label, help);
    ui.add(egui::TextEdit::singleline(value).desired_width(width));
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

/// Format the inclusive range for a hover/help hint, e.g. `1–10` or `0–1`
/// (floats trim trailing zeros via `format_numeric`, so `0.0`/`1.0` render as
/// `0`/`1`). Pure so it is unit-testable.
pub(in crate::ui) fn range_hint(bounds: &crate::config::NumericBounds) -> String {
    format!(
        "{}–{}",
        format_numeric(bounds.min, bounds.is_int),
        format_numeric(bounds.max, bounds.is_int)
    )
}

/// Edit-commit detection shared by the numeric fields: clamp the value to its
/// schema bounds only when the user actually finished editing (the field lost
/// focus — egui fires this after Enter or when focus moves elsewhere), never on
/// mere render. Mirrors the diagnostics combo's "only mutate on real user
/// change" discipline so it doesn't fight Save's dirty-state tracking: the
/// value is rewritten only when the clamped form differs from what's stored.
fn clamp_on_commit(
    response: &egui::Response,
    value: &mut String,
    bounds: &crate::config::NumericBounds,
) {
    if !response.lost_focus() {
        return;
    }
    let clamped = clamp_numeric_setting(
        value,
        bounds.min,
        bounds.max,
        bounds.is_int,
        &bounds.default,
    );
    if clamped != *value {
        *value = clamped;
    }
}

/// Numeric settings field (always-enabled). Looks up the schema bounds for
/// `key`, renders a compact text input, appends the range to the help text,
/// and clamps the value into `[min, max]` on edit-commit. Falls back to a plain
/// short text field if the key has no schema bounds (should not happen for the
/// wired numeric settings).
pub(in crate::ui) fn numeric_help(
    ui: &mut egui::Ui,
    lang: &str,
    key: &str,
    label: &str,
    value: &mut String,
    help: &str,
) {
    numeric_enabled(ui, lang, true, key, label, value, help);
}

/// Enabled-gated numeric settings field (see [`numeric_help`]). The field is
/// greyed out when `enabled` is false (e.g. an engine-specific knob whose
/// backend is not active), matching the existing `*_enabled` widgets.
pub(in crate::ui) fn numeric_enabled(
    ui: &mut egui::Ui,
    lang: &str,
    enabled: bool,
    key: &str,
    label: &str,
    value: &mut String,
    help: &str,
) {
    let bounds = crate::config::numeric_bounds(key);
    let help_text = match &bounds {
        // The "Range" word is localized so a Danish help string isn't followed
        // by an English suffix; the numbers themselves are language-neutral.
        Some(b) => format!(
            "{help} {}: {}.",
            ui_text(lang, UiTextKey::Range),
            range_hint(b)
        ),
        None => help.to_owned(),
    };
    let show_help = label_with_help_enabled(ui, enabled, label, &help_text);
    let mut response = None;
    ui.add_enabled_ui(enabled, |ui| {
        response = Some(
            ui.add(egui::TextEdit::singleline(value).desired_width(SETTINGS_SHORT_INPUT_WIDTH)),
        );
    });
    if let (Some(response), Some(bounds)) = (response, bounds) {
        clamp_on_commit(&response, value, &bounds);
    }
    ui.end_row();
    grid_help_row(ui, show_help, &help_text);
}

pub(in crate::ui) fn text_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    help: &str,
) {
    text_enabled_width(ui, enabled, label, value, help, SETTINGS_TEXT_INPUT_WIDTH);
}

/// Short-input variant of [`text_enabled`] for numeric/threshold fields.
pub(in crate::ui) fn text_enabled_short(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    help: &str,
) {
    text_enabled_width(ui, enabled, label, value, help, SETTINGS_SHORT_INPUT_WIDTH);
}

fn text_enabled_width(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    help: &str,
    width: f32,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(width));
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

pub(in crate::ui) fn password_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    reveal_until: &mut Option<Instant>,
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    let now = Instant::now();
    if reveal_until.is_some_and(|until| until <= now) {
        *reveal_until = None;
    }
    let is_revealed = reveal_until.is_some_and(|until| until > now);
    if let Some(until) = *reveal_until {
        ui.ctx()
            .request_repaint_after(until.saturating_duration_since(now));
    }
    ui.add_enabled_ui(enabled, |ui| {
        const PASSWORD_CONTROL_WIDTH: f32 = 360.0;
        const EYE_BUTTON_WIDTH: f32 = 26.0;
        const EYE_BUTTON_GAP: f32 = 4.0;
        let input_width = PASSWORD_CONTROL_WIDTH - EYE_BUTTON_WIDTH - EYE_BUTTON_GAP;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = EYE_BUTTON_GAP;
            ui.set_width(PASSWORD_CONTROL_WIDTH);
            // Render the field at its natural height and size the reveal button to
            // match, so the eye stays vertically centered on the same row instead
            // of drifting below the field (which made it look like it belonged to
            // the next field).
            let field = ui.add(
                egui::TextEdit::singleline(value)
                    .password(!is_revealed)
                    .desired_width(input_width),
            );
            let response = eye_icon_button(ui, is_revealed, field.rect.height()).on_hover_text(
                if is_revealed {
                    "Hide API key."
                } else {
                    "Show API key for 3 seconds."
                },
            );
            if response.clicked() {
                *reveal_until = if is_revealed {
                    None
                } else {
                    Some(Instant::now() + Duration::from_secs(3))
                };
            }
        });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

fn eye_icon_button(ui: &mut egui::Ui, active: bool, height: f32) -> egui::Response {
    let size = egui::vec2(26.0, height.max(18.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter()
            .rect(rect, 2.0, visuals.bg_fill, visuals.bg_stroke);

        let stroke = egui::Stroke::new(
            1.3,
            if active {
                ui.visuals().selection.stroke.color
            } else {
                visuals.fg_stroke.color
            },
        );
        // Fixed-size eye glyph centered in the button, independent of its height.
        let center = rect.center();
        let half_w = 8.0;
        let half_h = 5.0;
        let left = egui::pos2(center.x - half_w, center.y);
        let right = egui::pos2(center.x + half_w, center.y);
        let top = egui::pos2(center.x, center.y - half_h);
        let bottom = egui::pos2(center.x, center.y + half_h);
        ui.painter().line_segment([left, top], stroke);
        ui.painter().line_segment([top, right], stroke);
        ui.painter().line_segment([right, bottom], stroke);
        ui.painter().line_segment([bottom, left], stroke);
        ui.painter().circle_stroke(center, 2.4, stroke);
        if active {
            ui.painter()
                .circle_filled(center, 1.4, ui.visuals().selection.stroke.color);
        }
    }
    response
}

pub(in crate::ui) fn checkbox_help(ui: &mut egui::Ui, label: &str, value: &mut bool, help: &str) {
    let show_help = label_with_help(ui, label, help);
    ui.checkbox(value, "");
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

pub(in crate::ui) fn checkbox_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut bool,
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        ui.checkbox(value, "");
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

/// Selected-text label for a dynamic `(value, display)` combo: the matching
/// display, else the raw value, else `(empty)`. Pure so it is unit-testable.
pub(in crate::ui) fn dynamic_selected_label(value: &str, options: &[(String, String)]) -> String {
    options
        .iter()
        .find(|(option, _)| option == value)
        .map(|(_, display)| display.clone())
        .unwrap_or_else(|| {
            if value.is_empty() {
                "(empty)".to_owned()
            } else {
                value.to_owned()
            }
        })
}

pub(in crate::ui) fn selected_option_label(value: &str, options: &[(&str, &str)]) -> String {
    options
        .iter()
        .find(|(option, _)| *option == value)
        .map(|(_, display)| (*display).to_owned())
        .unwrap_or_else(|| {
            if value.is_empty() {
                "(empty)".to_owned()
            } else {
                value.to_owned()
            }
        })
}

pub(in crate::ui) fn labeled_options_contain(options: &[(&str, &str)], value: &str) -> bool {
    options.iter().any(|(option, _)| *option == value)
}

pub(in crate::ui) fn label_with_help(ui: &mut egui::Ui, label: &str, help: &str) -> bool {
    ui.horizontal(|ui| {
        // Label cell is the single alignment anchor for the settings grid: its
        // scaled min_width pins column 0 across all grids without a grid-wide floor.
        ui.set_min_width(settings_label_width(ui));
        let response = ui.label(label);
        if !help.is_empty() {
            response.on_hover_text(help);
        }
        help_badge(ui, label, help)
    })
    .inner
}

pub(in crate::ui) fn label_with_help_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    help: &str,
) -> bool {
    ui.horizontal(|ui| {
        // Label cell is the single alignment anchor for the settings grid: its
        // scaled min_width pins column 0 across all grids without a grid-wide floor.
        ui.set_min_width(settings_label_width(ui));
        let response = ui.add_enabled(enabled, egui::Label::new(label));
        if !help.is_empty() {
            response.on_hover_text(help);
        }
        help_badge(ui, label, help)
    })
    .inner
}

/// Width for the combo/text value controls, clamped to a readable range.
/// `pub(in crate::ui)` so `widgets_combo` (a sibling module) can use it.
pub(in crate::ui) fn settings_control_width(ui: &egui::Ui) -> f32 {
    ui.available_width()
        .clamp(260.0, SETTINGS_CONTROL_MAX_WIDTH)
}

/// Narrow, fixed width for short-enum combos (see [`SETTINGS_SHORT_CONTROL_WIDTH`]).
/// Scales with the UI text-scale like the label column does, and never exceeds the
/// available width so it degrades gracefully in a tight window. Unlike
/// [`settings_control_width`] it does NOT stretch to fill the row — a three-token
/// dropdown should stay compact.
pub(in crate::ui) fn settings_short_control_width(ui: &egui::Ui) -> f32 {
    let body_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .map(|f| f.size)
        .unwrap_or(14.0);
    let scaled = SETTINGS_SHORT_CONTROL_WIDTH * (body_size / 14.0);
    scaled.min(ui.available_width())
}

/// A standalone `?` help badge (outside the settings grid) that toggles an
/// inline help block, persisting its open/closed state by `id`. Returns whether
/// the help should currently show. Mirrors the grid rows' `?` affordance so help
/// is discoverable on button clusters (e.g. the System maintenance actions).
pub(in crate::ui) fn help_toggle_badge(ui: &mut egui::Ui, id: &str, help: &str) -> bool {
    if help.is_empty() {
        return false;
    }
    let persist_id = ui.make_persistent_id(("settings_help_toggle", id));
    let mut show_help = ui
        .data_mut(|data| data.get_persisted::<bool>(persist_id))
        .unwrap_or(false);
    let response = ui.small_button("?");
    if response.clicked() {
        show_help = !show_help;
        ui.data_mut(|data| data.insert_persisted(persist_id, show_help));
    }
    let _ = response.on_hover_text(help);
    show_help
}

fn help_badge(ui: &mut egui::Ui, label: &str, help: &str) -> bool {
    if help.is_empty() {
        return false;
    }

    let id = ui.make_persistent_id(("settings_help", label));
    let mut show_help = ui
        .data_mut(|data| data.get_persisted::<bool>(id))
        .unwrap_or(false);
    let response = ui.small_button("?");
    if response.clicked() {
        show_help = !show_help;
        ui.data_mut(|data| data.insert_persisted(id, show_help));
    }
    let _ = response.on_hover_text(help);
    show_help
}

pub(in crate::ui) fn grid_help_row(ui: &mut egui::Ui, show_help: bool, help: &str) {
    if show_help {
        ui.label("");
        inline_help(ui, true, help);
        ui.end_row();
    }
}

pub(in crate::ui) fn inline_help(ui: &mut egui::Ui, show_help: bool, help: &str) {
    if show_help {
        ui.add(
            egui::Label::new(egui::RichText::new(help).color(ui.visuals().weak_text_color()))
                .wrap(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_label_width_scales_proportionally_with_body_font_size() {
        // At scale 1.0 (body = 14.0 pt) → base width 220.
        let w = scaled_label_width(14.0);
        assert!((w - 220.0).abs() < 0.01, "scale 1.0 → {w}");

        // At scale 1.15 (body = 14.0 * 1.15 = 16.1 pt) → 220 * 1.15 = 253.
        let w = scaled_label_width(14.0 * 1.15);
        assert!((w - 253.0).abs() < 0.01, "scale 1.15 → {w}");

        // At max scale 1.6 (body = 14.0 * 1.6 = 22.4 pt) → 220 * 1.6 = 352.
        let w = scaled_label_width(14.0 * 1.6);
        assert!((w - 352.0).abs() < 0.01, "scale 1.6 → {w}");
    }

    #[test]
    fn clamp_numeric_int_in_range_is_canonicalized_unchanged() {
        assert_eq!(clamp_numeric_setting("5", 1.0, 10.0, true, "1"), "5");
        // A typed float on an int field rounds to the canonical integer string.
        assert_eq!(clamp_numeric_setting("5.0", 1.0, 10.0, true, "1"), "5");
        assert_eq!(clamp_numeric_setting(" 7 ", 1.0, 10.0, true, "1"), "7");
    }

    #[test]
    fn clamp_numeric_above_max_clamps_to_max() {
        // The motivating case: beam size 3000 -> 10.
        assert_eq!(clamp_numeric_setting("3000", 1.0, 10.0, true, "1"), "10");
        assert_eq!(clamp_numeric_setting("999", 0.0, 100.0, true, "30"), "100");
    }

    #[test]
    fn clamp_numeric_below_min_clamps_to_min() {
        assert_eq!(clamp_numeric_setting("0", 1.0, 10.0, true, "1"), "1");
        assert_eq!(clamp_numeric_setting("-50", 5.0, 600.0, true, "120"), "5");
    }

    #[test]
    fn clamp_numeric_garbage_falls_back_to_default_then_min() {
        // Unparseable input clamps the (parseable) default into range.
        assert_eq!(clamp_numeric_setting("abc", 1.0, 10.0, true, "1"), "1");
        // A default itself out of range is clamped too.
        assert_eq!(clamp_numeric_setting("xyz", 1.0, 10.0, true, "999"), "10");
        // Garbage with garbage default falls back to min.
        assert_eq!(clamp_numeric_setting("???", 1.0, 10.0, true, "???"), "1");
    }

    #[test]
    fn clamp_numeric_float_formatting_trims_zeros() {
        // Float field keeps decimals but trims trailing zeros.
        assert_eq!(clamp_numeric_setting("0.30", 0.0, 1.0, false, "0.3"), "0.3");
        assert_eq!(clamp_numeric_setting("0.5", 0.0, 5.0, false, "0.5"), "0.5");
        // Above max on a float clamps to the float max.
        assert_eq!(clamp_numeric_setting("9", 0.0, 1.0, false, "0.3"), "1");
        // A whole-valued float formats without a spurious ".0".
        assert_eq!(clamp_numeric_setting("3", 0.0, 30.0, false, "3"), "3");
    }

    #[test]
    fn clamp_numeric_rejects_non_finite() {
        // inf/NaN are not finite -> treated as garbage -> default.
        assert_eq!(clamp_numeric_setting("inf", 0.0, 1.0, false, "0.3"), "0.3");
        assert_eq!(clamp_numeric_setting("NaN", 1.0, 10.0, true, "1"), "1");
    }

    #[test]
    fn clamp_with_schema_default_snaps_garbage_to_default_not_min() {
        // FINDING 1 regression guard: garbage must clamp to the field's SCHEMA
        // DEFAULT (threaded via NumericBounds.default), not to its min. For
        // max_chars_per_second the default (30) differs from the min (0), so a
        // min-fallback would be the wrong, observable behaviour.
        let b = crate::config::numeric_bounds("max_chars_per_second")
            .expect("max_chars_per_second has bounds");
        assert_eq!(b.default, "30");
        assert_eq!(
            clamp_numeric_setting("garbage", b.min, b.max, b.is_int, &b.default),
            "30",
            "unparseable input should fall back to the schema default, not min"
        );
        // And a valid in-range value is still honoured (not overridden by default).
        assert_eq!(
            clamp_numeric_setting("12", b.min, b.max, b.is_int, &b.default),
            "12"
        );
    }

    #[test]
    fn range_hint_formats_int_and_float_bounds() {
        let int_b = crate::config::NumericBounds {
            min: 1.0,
            max: 10.0,
            step: 1.0,
            is_int: true,
            default: "5".to_owned(),
        };
        assert_eq!(range_hint(&int_b), "1–10");
        let float_b = crate::config::NumericBounds {
            min: 0.0,
            max: 1.0,
            step: 0.05,
            is_int: false,
            default: "0.3".to_owned(),
        };
        assert_eq!(range_hint(&float_b), "0–1");
    }
}
