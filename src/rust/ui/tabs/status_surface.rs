//! Compact status surface content and latest-transcript actions.

use super::super::*;
use super::*;
use crate::ui::tasks::{REINJECT_LAST_LABEL, RETRY_LAST_LABEL};
use egui_material_icons::icons;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum CompactStatus {
    Idle,
    Starting,
    Recording,
    Transcribing,
    PostProcessing,
    Injecting,
    Error,
}

impl CompactStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Starting => "Starting…",
            Self::Recording => "Recording",
            Self::Transcribing => "Transcribing…",
            Self::PostProcessing => "Post-processing…",
            Self::Injecting => "Injecting…",
            Self::Error => "Error",
        }
    }

    fn color(self, palette: UiPalette) -> egui::Color32 {
        match self {
            Self::Idle => palette.text_muted,
            Self::Starting | Self::Transcribing | Self::PostProcessing => palette.warn_text,
            Self::Recording | Self::Error => palette.error_text,
            Self::Injecting => palette.accent_blue,
        }
    }
}

pub(in crate::ui) fn compact_status_state(
    runtime_state: RuntimeState,
    worker_ready: bool,
    stage: Option<&'static str>,
    has_error: bool,
) -> CompactStatus {
    if has_error {
        return CompactStatus::Error;
    }
    match stage {
        Some("recording") => CompactStatus::Recording,
        Some("transcribing") => CompactStatus::Transcribing,
        Some("post-processing") => CompactStatus::PostProcessing,
        Some("injecting") => CompactStatus::Injecting,
        _ if runtime_state == RuntimeState::Stopped => CompactStatus::Idle,
        _ if !worker_ready => CompactStatus::Starting,
        _ => CompactStatus::Idle,
    }
}

pub(in crate::ui) fn compact_status_label(
    ui: &mut egui::Ui,
    status: CompactStatus,
    palette: UiPalette,
) {
    ui.label(
        egui::RichText::new(status.label())
            .strong()
            .color(status.color(palette)),
    );
}

pub(in crate::ui) fn compact_status_color(
    status: CompactStatus,
    palette: UiPalette,
) -> egui::Color32 {
    status.color(palette)
}

impl WhisperDictateApp {
    pub(in crate::ui) fn compact_metadata(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let (_, _, model) = self.stt_detail_summary();
        let profile = self.active_profile.as_deref().unwrap_or("Default profile");
        ui.add_space(5.0);
        ui.label(
            egui::RichText::new(format!(
                "{} · model: {} · profile: {}",
                self.backend_summary(),
                model,
                profile,
            ))
            .small()
            .color(palette.text_muted),
        );
    }

    /// Show the newest transcript with actions that keep the target window in
    /// control: copying uses egui's clipboard bridge, while reinjection uses
    /// the existing background-task lane.
    pub(in crate::ui) fn last_transcript_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let text = self
            .last_transcript
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        if text.is_none() && self.last_runtime_error.is_none() {
            return;
        }
        ui.add_space(6.0);
        if let Some(text) = text {
            egui::Frame::default()
                .fill(palette.readout_bg)
                .stroke(egui::Stroke::new(0.8, palette.border_soft))
                .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Last transcript")
                                .strong()
                                .color(palette.text),
                        );
                        ui.add_space(5.0);
                        let label_width = ui.available_width().min(460.0).max(80.0);
                        ui.add_sized(
                            egui::vec2(label_width, 20.0),
                            egui::Label::new(
                                egui::RichText::new(compact_label(&text, 72)).color(palette.text),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&text);
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(egui::RichText::new(icons::ICON_COPY_ALL.codepoint))
                            .clicked()
                        {
                            ui.ctx().copy_text(text.clone());
                        }
                        if ui.button("Reinject").clicked() {
                            self.run_reinject_last(REINJECT_LAST_LABEL);
                        }
                        if ui.button("Retry").clicked() {
                            self.run_reinject_last(RETRY_LAST_LABEL);
                        }
                        if ui.button("Dictionary").clicked() {
                            self.set_compact_mode(ui.ctx(), false);
                            self.selected_tab = Tab::Dictionary;
                        }
                        if ui.button("Settings").clicked() {
                            self.set_compact_mode(ui.ctx(), false);
                            self.selected_tab = Tab::Speech;
                        }
                    });
                });
        }
        if let Some(error) = self.last_runtime_error.as_deref() {
            ui.add(
                egui::Label::new(egui::RichText::new(error).small().color(palette.error_text))
                    .wrap(),
            );
        }
    }
}
