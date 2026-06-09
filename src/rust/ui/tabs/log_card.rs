//! Frame widgets for the runtime log view: the per-entry log "cards", the empty
//! state, the small status pill, and the session metric boxes.

use super::super::*;
use super::*;
use egui_material_icons::icons;

/// Icon glyph, accent colour and frame fill for a card kind.
fn runtime_log_card_style(
    kind: RuntimeLogCardKind,
    palette: UiPalette,
) -> (&'static str, egui::Color32, egui::Color32) {
    match kind {
        RuntimeLogCardKind::FinalText => {
            (icons::ICON_CHECK_CIRCLE, palette.ok_text, palette.surface_active_bg)
        }
        RuntimeLogCardKind::Status => (icons::ICON_INFO, palette.accent_blue, palette.surface_bg),
        RuntimeLogCardKind::Diagnostic => {
            (icons::ICON_GRAPHIC_EQ, palette.warn_text, palette.header_bg)
        }
    }
}

/// Title line plus the optional detail/badge row (the card's right column).
fn runtime_log_card_text(
    ui: &mut egui::Ui,
    card: &RuntimeLogCard,
    accent: egui::Color32,
    palette: UiPalette,
) {
    let title_size = match card.kind {
        RuntimeLogCardKind::FinalText => 17.0,
        _ => 14.0,
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(&card.title)
                .size(title_size)
                .strong()
                .color(palette.text),
        )
        .wrap(),
    );
    if card.detail.is_empty() && card.badge.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if !card.detail.is_empty() {
            ui.label(
                egui::RichText::new(&card.detail)
                    .size(12.0)
                    .color(palette.text_muted),
            );
        }
        if !card.badge.is_empty() {
            status_pill(ui, &card.badge, accent, palette);
        }
    });
}

pub(in crate::ui) fn runtime_log_card(
    ui: &mut egui::Ui,
    card: &RuntimeLogCard,
    palette: UiPalette,
) {
    let (icon, accent, fill) = runtime_log_card_style(card.kind, palette);
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(12.0, 12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                egui::Frame::default()
                    .fill(accent)
                    .rounding(egui::Rounding::same(PILL_RADIUS as f32))
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(4.0, 46.0));
                    });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(icon).size(20.0).color(accent));
                ui.vertical(|ui| runtime_log_card_text(ui, card, accent, palette));
            });
        });
}

pub(in crate::ui) fn empty_log_state(
    ui: &mut egui::Ui,
    state: RuntimeState,
    palette: UiPalette,
    raw_language: &str,
) {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                icon_text(
                    icons::ICON_MIC,
                    ui_text(raw_language, UiTextKey::NoDictationOutputYet),
                )
                .strong()
                .color(palette.text),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    ui_text(raw_language, UiTextKey::RuntimeStatus),
                    runtime_state_label(state, raw_language)
                ))
                .size(12.0)
                .color(palette.text_muted),
            );
        });
}

fn status_pill(ui: &mut egui::Ui, label: &str, accent: egui::Color32, palette: UiPalette) {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, accent))
        .rounding(egui::Rounding::same(PILL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(11.0).strong().color(accent));
        });
}

pub(in crate::ui) fn metric_box(
    ui: &mut egui::Ui,
    label: &str,
    value: impl AsRef<str>,
    palette: UiPalette,
) {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(CONTROL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(11.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width(102.0);
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(palette.text_muted),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(value.as_ref())
                        .size(12.0)
                        .color(palette.text),
                )
                .wrap(),
            );
        });
}
