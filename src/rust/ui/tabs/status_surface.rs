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
    fn text_key(self) -> UiTextKey {
        match self {
            Self::Idle => UiTextKey::CompactIdle,
            Self::Starting => UiTextKey::CompactStarting,
            Self::Recording => UiTextKey::CompactRecording,
            Self::Transcribing => UiTextKey::CompactTranscribing,
            Self::PostProcessing => UiTextKey::CompactPostProcessing,
            Self::Injecting => UiTextKey::CompactInjecting,
            Self::Error => UiTextKey::CompactError,
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
    match stage {
        Some("recording") => CompactStatus::Recording,
        Some("transcribing") => CompactStatus::Transcribing,
        Some("post-processing") => CompactStatus::PostProcessing,
        Some("injecting") => CompactStatus::Injecting,
        _ if has_error => CompactStatus::Error,
        _ if runtime_state == RuntimeState::Stopped => CompactStatus::Idle,
        _ if !worker_ready => CompactStatus::Starting,
        _ => CompactStatus::Idle,
    }
}

pub(in crate::ui) fn compact_status_label(
    ui: &mut egui::Ui,
    status: CompactStatus,
    palette: UiPalette,
    language: &str,
) {
    ui.add_sized(
        egui::vec2(128.0, 22.0),
        egui::Label::new(
            egui::RichText::new(ui_text(language, status.text_key()))
                .strong()
                .color(status.color(palette)),
        ),
    );
}

pub(in crate::ui) fn compact_status_color(
    status: CompactStatus,
    palette: UiPalette,
) -> egui::Color32 {
    status.color(palette)
}

pub(in crate::ui) fn transcript_actions_enabled(
    pipeline_stage: Option<&str>,
    background_task_running: bool,
    inject_mode: &str,
    target_activation_available: bool,
) -> bool {
    pipeline_stage.is_none()
        && !background_task_running
        && !inject_mode.trim().eq_ignore_ascii_case("print")
        && target_activation_available
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(in crate::ui) fn target_activation_available_for(
    wayland_display: bool,
    x11_display: bool,
) -> bool {
    !(wayland_display && !x11_display)
}

pub(in crate::ui) fn retained_target_available(
    target_id: &str,
    target_title: &str,
    target_process: &str,
) -> bool {
    [target_id, target_title, target_process]
        .iter()
        .any(|value| !value.trim().is_empty())
}

fn target_activation_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(all(not(target_os = "macos"), target_os = "linux"))]
    {
        target_activation_available_for(
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
            std::env::var_os("DISPLAY").is_some(),
        )
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
    {
        true
    }
}

impl WhisperDictateApp {
    pub(in crate::ui) fn compact_metadata_model(&self) -> String {
        match SttBackendMode::from_raw(&self.settings.stt_backend) {
            SttBackendMode::Cloud => self.stt_detail_summary().2,
            SttBackendMode::Whisper => {
                let model = self.settings.model.trim();
                if model.is_empty() {
                    ui_text(&self.settings.ui_language, UiTextKey::NotConfigured).to_owned()
                } else {
                    compact_label(model, 28)
                }
            }
        }
    }

    pub(in crate::ui) fn compact_metadata(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let model = self.compact_metadata_model();
        let language = &self.settings.ui_language;
        let profile = self
            .active_profile
            .as_deref()
            .unwrap_or_else(|| ui_text(language, UiTextKey::DefaultProfile));
        ui.add_space(5.0);
        ui.label(
            egui::RichText::new(format!(
                "{} · {}: {} · {}: {}",
                self.backend_summary(),
                ui_text(language, UiTextKey::CompactModel),
                model,
                ui_text(language, UiTextKey::CompactProfile),
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
        let transcript = self
            .last_transcript
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned);
        if transcript.is_none() && self.last_runtime_error.is_none() {
            return;
        }
        ui.add_space(6.0);
        if let Some(text) = transcript {
            let display_text = text.trim();
            egui::Frame::default()
                .fill(palette.readout_bg)
                .stroke(egui::Stroke::new(0.8, palette.border_soft))
                .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(ui_text(
                                &self.settings.ui_language,
                                UiTextKey::LastTranscript,
                            ))
                            .strong()
                            .color(palette.text),
                        );
                        ui.add_space(5.0);
                        let label_width = ui.available_width().clamp(80.0, 460.0);
                        ui.add_sized(
                            egui::vec2(label_width, 20.0),
                            egui::Label::new(
                                egui::RichText::new(compact_label(display_text, 72))
                                    .color(palette.text),
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
                            crate::injection::cancel_ui_clipboard_restore();
                            ui.ctx().copy_text(text.clone());
                        }
                        let inject_mode = self
                            .last_inject_mode
                            .as_deref()
                            .unwrap_or(self.settings.inject_mode.as_str());
                        let can_reinject = transcript_actions_enabled(
                            self.pipeline_stage,
                            self.background_task.is_some(),
                            inject_mode,
                            target_activation_available()
                                && retained_target_available(
                                    &self.last_target_id,
                                    &self.last_target_title,
                                    &self.last_target_process,
                                ),
                        );
                        if ui
                            .add_enabled(
                                can_reinject,
                                egui::Button::new(ui_text(
                                    &self.settings.ui_language,
                                    UiTextKey::Reinject,
                                )),
                            )
                            .clicked()
                        {
                            self.run_reinject_last(REINJECT_LAST_LABEL);
                        }
                        if ui
                            .add_enabled(
                                can_reinject,
                                egui::Button::new(ui_text(
                                    &self.settings.ui_language,
                                    UiTextKey::Retry,
                                )),
                            )
                            .clicked()
                        {
                            self.run_reinject_last(RETRY_LAST_LABEL);
                        }
                        if ui
                            .button(ui_text(&self.settings.ui_language, UiTextKey::Dictionary))
                            .clicked()
                        {
                            self.set_compact_mode(ui.ctx(), false);
                            self.selected_tab = Tab::Dictionary;
                        }
                        if ui
                            .button(ui_text(&self.settings.ui_language, UiTextKey::Settings))
                            .clicked()
                        {
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
