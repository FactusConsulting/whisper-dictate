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

fn text_help_width(ui: &mut egui::Ui, label: &str, value: &mut String, help: &str, width: f32) {
    let show_help = label_with_help(ui, label, help);
    ui.add(egui::TextEdit::singleline(value).desired_width(width));
    ui.end_row();
    grid_help_row(ui, show_help, help);
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
}
