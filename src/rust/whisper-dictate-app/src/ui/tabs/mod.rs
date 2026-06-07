use super::*;
use egui_material_icons::icons;

mod dictionary;
mod output;
mod post;
mod profiles;
mod quality;
mod runtime;
mod settings;
mod shell;
mod speech;

// Free helpers consumed by sibling test modules through `ui::tabs::NAME`.
#[cfg(test)]
pub(in crate::ui) use runtime::{
    audio_device_label, empty_as_auto, empty_as_disabled, full_audio_device_label,
    live_audio_level_summary, mic_label_char_budget,
};
#[cfg(test)]
pub(in crate::ui) use settings::reset_tab_settings;

fn settings_grid(id: &'static str) -> egui::Grid {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(20.0, 10.0))
}

fn section_label(ui: &mut egui::Ui, label: &str, palette: UiPalette) {
    ui.label(
        egui::RichText::new(label)
            .size(12.0)
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

fn language_toggle(ui: &mut egui::Ui, value: &mut String, palette: UiPalette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (raw, label) in [
            ("en", ui_text(value.as_str(), UiTextKey::English)),
            ("da", ui_text(value.as_str(), UiTextKey::Danish)),
        ] {
            let selected = value == raw;
            let fill = if selected {
                palette.accent_dark
            } else {
                palette.surface_bg
            };
            let text = if selected {
                egui::RichText::new(label).strong().color(palette.text)
            } else {
                egui::RichText::new(label).color(palette.text_muted)
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

fn status_label(ui: &mut egui::Ui, text: &str, palette: UiPalette) {
    let rich_text = if text.starts_with("[OK]") {
        egui::RichText::new(text).color(palette.ok_text)
    } else if text.starts_with("[ERROR]") {
        egui::RichText::new(text).color(palette.error_text)
    } else {
        egui::RichText::new(text)
    };
    ui.add(egui::Label::new(rich_text).wrap());
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
