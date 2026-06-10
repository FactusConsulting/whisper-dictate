use super::*;
use egui_material_icons::icons;

mod compact;
mod dictionary;
mod log_card;
mod output;
mod post;
mod profiles;
mod quality;
mod runtime;
mod settings;
mod shell;
mod speech;
mod system;

// Log-card render widgets used by the runtime tab; re-exported into the `tabs`
// root so `runtime` (and any future tab) can reach them via `super::*`.
#[cfg(test)]
pub(in crate::ui) use log_card::drag_overshoot_delta;
pub(in crate::ui) use log_card::{drag_autoscroll, empty_log_state, metric_box, runtime_log_card};
// The live mic gauge, runtime-state colour, pipeline-progress accent, and the
// mic-label/audio-summary helpers are shared between the full runtime tab /
// top status bar and the compact strip (`compact.rs`).
pub(in crate::ui) use runtime::{
    audio_device_label, full_audio_device_label, level_gauge, live_audio_level_summary,
    pipeline_progress_accent_color,
};
pub(in crate::ui) use shell::runtime_state_color;
// The default-metrics-path helper is reused at app construction (ui.rs) to
// prefill the empty Metrics JSONL field, so re-export it from the tabs root.
pub(in crate::ui) use system::default_metrics_jsonl_path;
// The hotkey-chord formatters are shared with the sidebar's key display
// (`shell.rs`) and the sibling test modules.
pub(in crate::ui) use runtime::{format_push_to_talk_keys, push_to_talk_badge_label};

// Free helpers consumed by sibling test modules through `ui::tabs::NAME`.
#[cfg(test)]
pub(in crate::ui) use runtime::{empty_as_auto, empty_as_disabled, mic_label_char_budget};
#[cfg(test)]
pub(in crate::ui) use settings::reset_tab_settings;
#[cfg(test)]
pub(in crate::ui) use shell::{
    post_indicator_hover, post_indicator_label, post_processing_enabled, top_status_controls_width,
    top_status_left_width,
};

fn settings_grid(id: &'static str) -> egui::Grid {
    // Alignment is anchored by the label cell's `set_min_width(settings_label_width(ui))`
    // call inside `label_with_help` / `label_with_help_enabled` — no grid-wide
    // column floor is needed, and omitting it avoids an unscaled floor on the
    // value column.
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(20.0, 10.0))
}

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
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
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
                icons::ICON_DARK_MODE,
                ui_text(raw_language, UiTextKey::Dark),
            ),
            (
                "light",
                icons::ICON_LIGHT_MODE,
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
