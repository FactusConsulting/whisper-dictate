use super::*;
use egui_material_icons::icons;

mod api_check_indicator;
mod benchmark_results;
mod compact;
mod corpus_record;
mod dictionary;
mod log_card;
mod output;
mod pipeline_progress;
mod post;
mod profiles;
mod quality;
mod runtime;
mod runtime_format;
mod settings;
mod shell;
mod shell_indicator;
#[cfg(test)]
mod shell_sidebar_tests;
mod speech;
mod system;
mod top_status_layout;
mod update_badge;
mod whisper_models;

// The inline Test-API ✓/✗/testing indicator render shell (post.rs + speech.rs
// reach it via `super::*`). The pure classifier + parts builder it wraps are
// unit-tested inside the module itself.
pub(in crate::ui) use api_check_indicator::render_api_check_indicator;
// Log-card render widgets used by the runtime tab; re-exported into the `tabs`
// root so `runtime` (and any future tab) can reach them via `super::*`.
#[cfg(test)]
pub(in crate::ui) use log_card::drag_overshoot_delta;
pub(in crate::ui) use log_card::{drag_autoscroll, empty_log_state, metric_box, runtime_log_card};
// The live mic gauge, runtime-state colour, pipeline-progress accent, and the
// mic-label/audio-summary helpers are shared between the full runtime tab /
// top status bar and the compact strip (`compact.rs`).
pub(in crate::ui) use pipeline_progress::pipeline_progress_accent_color;
pub(in crate::ui) use runtime::level_gauge;
pub(in crate::ui) use runtime_format::{
    audio_device_label, full_audio_device_label, live_audio_level_summary,
};
pub(in crate::ui) use shell::runtime_state_color;
pub(in crate::ui) use shell_indicator::recording_indicator_style;
// Pure top-status-bar layout + post-indicator helpers live in their own module
// (`top_status_layout.rs`) so the render code in `shell.rs` stays small. The
// post on/off + label/hover helpers are used by `shell.rs`'s render code via
// `super::*`; the fit/width budget fns and these are also re-exported to the
// sibling test modules below.
pub(in crate::ui) use top_status_layout::{
    post_indicator_hover, post_indicator_label, post_processing_enabled, top_status_cards_fit,
    top_status_controls_width, top_status_left_width,
};
// The default-metrics-path helper is reused at app construction (ui.rs) to
// prefill the empty Metrics JSONL field, so re-export it from the tabs root.
pub(in crate::ui) use system::default_metrics_jsonl_path;
// The hotkey-chord formatters are shared with the sidebar's key display
// (`shell.rs`) and the sibling test modules.
pub(in crate::ui) use runtime_format::{format_push_to_talk_keys, push_to_talk_badge_label};

// Pure runtime summary/format helpers used by the runtime tab's render code
// (reached via `super::*`) and the sibling test modules.
pub(in crate::ui) use runtime_format::{
    empty_as_auto, empty_as_disabled, gauge_color_for_position, latest_log_summary,
    latest_metric_summary, mic_label_char_budget,
};
#[cfg(test)]
pub(in crate::ui) use settings::reset_tab_settings;
fn settings_grid(id: &'static str) -> egui::Grid {
    // Alignment is anchored by the label cell's `set_min_width(settings_label_width(ui))`
    // call inside `label_with_help` / `label_with_help_enabled` — no grid-wide
    // column floor is needed, and omitting it avoids an unscaled floor on the
    // value column.
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(20.0, 10.0))
}

/// Horizontal and vertical inner margin used by [`scope_group`] frames.
/// Referenced here AND in `speech.rs` so the standalone Speech-engine selector
/// gets the identical left inset without drawing a box (approach B).
// egui 0.34 `Margin` fields are `i8` (was `f32`); these are only ever used as
// `Margin::symmetric` arguments, so they are typed `i8` to match.
pub(in crate::ui) const SCOPE_GROUP_INNER_MARGIN_H: i8 = 12;
pub(in crate::ui) const SCOPE_GROUP_INNER_MARGIN_V: i8 = 10;

/// A thin outlined box grouping a set of settings under a small heading. Used by
/// the Quality tab to separate "All backends" / "Whisper" / "Parakeet" scopes so
/// it is obvious which engine each knob affects. The body runs inside its own
/// 2-column settings grid (unique `grid_id`), keeping the labelled-row layout.
fn scope_group(
    ui: &mut egui::Ui,
    palette: UiPalette,
    heading: &str,
    grid_id: &'static str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::default()
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
        .inner_margin(egui::Margin::symmetric(
            SCOPE_GROUP_INNER_MARGIN_H,
            SCOPE_GROUP_INNER_MARGIN_V,
        ))
        .show(ui, |ui| {
            section_label(ui, heading, palette);
            ui.add_space(6.0);
            settings_grid(grid_id).show(ui, add_contents);
        });
}

fn section_label(ui: &mut egui::Ui, label: &str, palette: UiPalette) {
    // Size comes from the centralized `TextStyle::Small` so all section headers
    // scale together with the UI text-scale setting.
    ui.label(
        egui::RichText::new(label)
            .text_style(egui::TextStyle::Small)
            .strong()
            .color(palette.text_muted),
    );
}

fn theme_toggle(ui: &mut egui::Ui, value: &mut String, palette: UiPalette, raw_language: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (raw, icon, label) in [
            (
                "dark",
                icons::ICON_DARK_MODE.codepoint,
                ui_text(raw_language, UiTextKey::Dark),
            ),
            (
                "light",
                icons::ICON_LIGHT_MODE.codepoint,
                ui_text(raw_language, UiTextKey::Light),
            ),
        ] {
            let selected = value == raw;
            let fill = if selected {
                palette.accent_dark
            } else {
                palette.surface_bg
            };
            let text = if selected {
                icon_text(icon, label).strong().color(palette.text)
            } else {
                icon_text(icon, label).color(palette.text_muted)
            };
            if ui
                .add_sized(
                    egui::vec2(92.0, 30.0),
                    egui::Button::new(text)
                        .fill(fill)
                        .stroke(egui::Stroke::new(0.8, palette.border_soft)),
                )
                .clicked()
            {
                *value = raw.to_owned();
            }
        }
    });
}

pub(in crate::ui) fn compact_label(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return value.to_owned();
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}
