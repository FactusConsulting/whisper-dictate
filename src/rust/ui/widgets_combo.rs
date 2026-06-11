//! ComboBox helper rows for the settings grid. Extracted from `widgets.rs` so
//! that file stays under the 500-line module size limit while keeping all
//! combo-specific rendering in one cohesive place.

use super::*;

/// Width policy for a settings combo. WIDE combos stretch to fill the row (good
/// for long descriptive option labels like model pickers / compute type); SHORT
/// combos use a fixed narrow width so a three-token dropdown
/// (auto/type/paste, Off/Basic/Verbose) doesn't sprawl across the whole grid.
#[derive(Clone, Copy)]
pub(in crate::ui) enum ComboWidth {
    Wide,
    Short,
}

impl ComboWidth {
    fn px(self, ui: &egui::Ui) -> f32 {
        match self {
            ComboWidth::Wide => settings_control_width(ui),
            ComboWidth::Short => settings_short_control_width(ui),
        }
    }
}

/// Narrow combo over a flat `&[&str]` option list, for short-enum values such as
/// Inject mode (auto/type/paste) or Format commands (off/en/da/both). The plain
/// `&[&str]` combos in this codebase are all short tokens, so this is the only
/// public flavour; long descriptive option labels use the `*_labeled` helpers.
pub(in crate::ui) fn combo_help_short(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
) {
    let show_help = label_with_help(ui, label, help);
    egui::ComboBox::from_id_salt(label)
        .width(ComboWidth::Short.px(ui))
        .selected_text(if value.is_empty() {
            "(empty)"
        } else {
            value.as_str()
        })
        .show_ui(ui, |ui| {
            for option in options {
                ui.selectable_value(
                    value,
                    (*option).to_owned(),
                    if option.is_empty() { "(empty)" } else { option },
                );
            }
        });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

pub(in crate::ui) fn combo_help_labeled(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    combo_help_labeled_w(ui, label, value, options, help, ComboWidth::Wide);
}

/// Narrow variant of [`combo_help_labeled`] for short display labels.
pub(in crate::ui) fn combo_help_labeled_short(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    combo_help_labeled_w(ui, label, value, options, help, ComboWidth::Short);
}

fn combo_help_labeled_w(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
    width: ComboWidth,
) {
    let show_help = label_with_help(ui, label, help);
    egui::ComboBox::from_id_salt(label)
        .width(width.px(ui))
        .selected_text(selected_option_label(value, options))
        .show_ui(ui, |ui| {
            for (option, display) in options {
                ui.selectable_value(value, (*option).to_owned(), *display);
            }
        });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

pub(in crate::ui) fn combo_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
) {
    combo_enabled_w(ui, enabled, label, value, options, help, ComboWidth::Wide);
}

/// Narrow variant of [`combo_enabled`] for short-enum options.
pub(in crate::ui) fn combo_enabled_short(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
) {
    combo_enabled_w(ui, enabled, label, value, options, help, ComboWidth::Short);
}

#[allow(clippy::too_many_arguments)] // a labelled, enabled, width-policy form row needs them
fn combo_enabled_w(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[&str],
    help: &str,
    width: ComboWidth,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(label)
            .width(width.px(ui))
            .selected_text(if value.is_empty() {
                "(empty)"
            } else {
                value.as_str()
            })
            .show_ui(ui, |ui| {
                for option in options {
                    ui.selectable_value(
                        value,
                        (*option).to_owned(),
                        if option.is_empty() { "(empty)" } else { option },
                    );
                }
            });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

pub(in crate::ui) fn combo_enabled_labeled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    combo_enabled_labeled_w(ui, enabled, label, value, options, help, ComboWidth::Wide);
}

/// Narrow variant of [`combo_enabled_labeled`] for short display labels.
pub(in crate::ui) fn combo_enabled_labeled_short(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
) {
    combo_enabled_labeled_w(ui, enabled, label, value, options, help, ComboWidth::Short);
}

#[allow(clippy::too_many_arguments)] // a labelled, enabled, width-policy form row needs them
fn combo_enabled_labeled_w(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    options: &[(&str, &str)],
    help: &str,
    width: ComboWidth,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(label)
            .width(width.px(ui))
            .selected_text(selected_option_label(value, options))
            .show_ui(ui, |ui| {
                for (option, display) in options {
                    ui.selectable_value(value, (*option).to_owned(), *display);
                }
            });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

/// Model picker with accuracy/speed annotations and VRAM-aware grey-out.
///
/// `hint(value) -> (note, approx_mb)`. When `gpu_total_mb` is `Some`, options
/// needing more VRAM than the GPU has total are disabled (they can't fit even
/// with an idle GPU) and explain why on hover. With `None` (CPU / non-NVIDIA)
/// nothing is greyed out — every model runs, large ones just slower.
#[allow(clippy::too_many_arguments)] // a labelled, VRAM-gated form row genuinely needs them
pub(in crate::ui) fn combo_model_vram(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut String,
    models: &[&str],
    hint: fn(&str) -> (&'static str, u32),
    gpu_total_mb: Option<u32>,
    help: &str,
) {
    let show_help = label_with_help_enabled(ui, enabled, label, help);
    let (cur_note, _) = hint(value);
    let selected_text = if value.is_empty() {
        "(empty)".to_owned()
    } else if cur_note.is_empty() {
        value.clone()
    } else {
        format!("{value} — {cur_note}")
    };
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(label)
            .selected_text(selected_text)
            .width(settings_control_width(ui))
            .show_ui(ui, |ui| {
                for model in models {
                    let (note, mb) = hint(model);
                    let fits = gpu_total_mb.is_none_or(|total| mb <= total);
                    let display = if note.is_empty() {
                        (*model).to_owned()
                    } else {
                        format!("{model} — {note} (~{mb} MB)")
                    };
                    let selected = value.as_str() == *model;
                    let response =
                        ui.add_enabled(fits, egui::SelectableLabel::new(selected, display));
                    let response = match gpu_total_mb {
                        Some(total) if !fits => response.on_disabled_hover_text(format!(
                            "Needs about {mb} MB VRAM; this GPU has {total} MB total."
                        )),
                        _ => response,
                    };
                    if response.clicked() {
                        *value = (*model).to_owned();
                    }
                }
            });
    });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}

/// A labelled combo over a dynamically built `(value, display)` list (owned
/// strings), as opposed to the `&'static` tables `combo_help_labeled` takes.
/// Used by the Microphone picker whose options come from the worker at runtime.
pub(in crate::ui) fn combo_help_dynamic(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(String, String)],
    help: &str,
) {
    let show_help = label_with_help(ui, label, help);
    egui::ComboBox::from_id_salt(label)
        .width(settings_control_width(ui))
        .selected_text(dynamic_selected_label(value, options))
        .show_ui(ui, |ui| {
            for (option, display) in options {
                ui.selectable_value(value, option.clone(), display);
            }
        });
    ui.end_row();
    grid_help_row(ui, show_help, help);
}
