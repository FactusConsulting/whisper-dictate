use super::super::*;
use super::*;
use egui_material_icons::icons;
use std::time::Duration;

const MIC_INDICATOR_MAX_WIDTH: f32 = 330.0;
const MIC_INDICATOR_MIN_WIDTH: f32 = 150.0;
const MIC_GAUGE_MAX_WIDTH: f32 = 150.0;
const MIC_GAUGE_MIN_WIDTH: f32 = 86.0;
const RUNTIME_LOG_TOP_MARGIN: f32 = 16.0;
const RUNTIME_LOG_CONTENT_TOP_PADDING: f32 = 10.0;
const RUNTIME_LOG_CONTENT_BOTTOM_PADDING: f32 = 14.0;

impl WhisperDictateApp {
    pub(in crate::ui) fn runtime_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        self.live_dictation_panel(ui, palette);
    }

    fn live_dictation_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.horizontal(|ui| {
            ui.label(
                icon_text(
                    icons::ICON_MIC.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::LiveDictation),
                )
                .size(18.0)
                .strong()
                .color(palette.text),
            );
            runtime_status_badge(
                ui,
                self.display_runtime_state(),
                palette,
                &self.settings.ui_language,
            );
            // The hotkey chord moved to the sidebar (above Save settings) — the
            // header keeps only status + mic so long previews get the width.
            let mic_width = (ui.available_width() - 10.0).clamp(0.0, MIC_INDICATOR_MAX_WIDTH);
            if mic_width >= MIC_INDICATOR_MIN_WIDTH {
                ui.add_space(10.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(mic_width, 30.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        self.listening_gauge(ui, palette, mic_width);
                    },
                );
            }
        });
        self.device_unusable_banner(ui, palette);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(ui_text(
                &self.settings.ui_language,
                UiTextKey::DictationOutput,
            ));
            self.log_mode_selector(ui, palette);
            self.runtime_log_actions(ui);
        });
        ui.add_space(10.0);

        // Fill the remaining height so the log card ends at the panel's content
        // bottom (a uniform EDGE_MARGIN gap via the CentralPanel) instead of being
        // forced taller than the window and overflowing below the bottom edge.
        let log_height = (ui.available_height() - (RUNTIME_LOG_TOP_MARGIN + 10.0)).max(0.0);
        let visible_log = self.visible_runtime_log();
        runtime_log_frame(palette).show(ui, |ui| {
            ui.set_min_height(log_height);
            egui::ScrollArea::vertical()
                .id_salt("runtime_log_scroll")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(log_height)
                .show(ui, |ui| {
                    // egui does not auto-scroll while drag-selecting text past
                    // the viewport edge — without this, a selection stops dead
                    // at the bottom/top of the box.
                    drag_autoscroll(ui);
                    self.render_log_entries(ui, palette, &visible_log);
                });
        });
    }

    /// A prominent red banner shown when the worker reports that the selected
    /// microphone is unusable (a `status` event with
    /// `state="error"`/`reason="device_unusable"`). The user picked a mic that
    /// can't be opened on any audio backend and must SEE that without opening the
    /// Debug log. Nothing is rendered when `device_error` is `None` (cleared on a
    /// subsequent working device / start / stop / exit).
    fn device_unusable_banner(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let Some(message) = self.device_error.as_deref() else {
            return;
        };
        ui.add_space(8.0);
        egui::Frame::default()
            .fill(palette.surface_active_bg)
            .stroke(egui::Stroke::new(1.0, palette.error_text))
            .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        icon_text(
                            icons::ICON_ERROR.codepoint,
                            ui_text(&self.settings.ui_language, UiTextKey::DeviceUnusableTitle),
                        )
                        .strong()
                        .color(palette.error_text),
                    );
                });
                ui.add_space(4.0);
                ui.add(egui::Label::new(egui::RichText::new(message).color(palette.text)).wrap());
            });
    }

    fn runtime_log_actions(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(icon_text(
                    icons::ICON_COPY_ALL.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::Copy),
                ))
                .clicked()
            {
                ui.ctx().copy_text(self.visible_runtime_log());
            }
            if ui
                .button(icon_text(
                    icons::ICON_DELETE.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::Clear),
                ))
                .clicked()
            {
                self.runtime_log.clear();
                self.runtime_log_scroll_to_bottom = true;
            }
        });
    }

    fn render_log_entries(&mut self, ui: &mut egui::Ui, palette: UiPalette, visible_log: &str) {
        ui.set_min_width(ui.available_width());
        ui.add_space(RUNTIME_LOG_CONTENT_TOP_PADDING);
        if self.runtime_log_view == LogViewMode::Debug {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(visible_log)
                        .monospace()
                        .color(palette.text),
                )
                .selectable(true)
                .wrap(),
            );
        } else {
            self.render_log_cards(ui, palette);
        }
        self.render_pipeline_progress(ui, palette);
        let bottom = ui.allocate_response(
            egui::vec2(ui.available_width(), RUNTIME_LOG_CONTENT_BOTTOM_PADDING),
            egui::Sense::hover(),
        );
        if self.runtime_log_scroll_to_bottom {
            bottom.scroll_to_me(Some(egui::Align::BOTTOM));
            self.runtime_log_scroll_to_bottom = false;
        }
    }

    /// Live card for the in-flight utterance: spins through the pipeline stages
    /// (recording → transcribing → post-processing) so slow CPU runs show
    /// progress. Cleared once the utterance settles into its Final card.
    fn render_pipeline_progress(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let Some(stage) = self.pipeline_stage else {
            return;
        };
        let label = match stage {
            "recording" => "Recording…",
            "transcribing" => "Transcribing…",
            "post-processing" => "Post-processing…",
            _ => return,
        };
        let accent = pipeline_progress_accent_color(stage, palette);
        egui::Frame::default()
            .fill(palette.surface_active_bg)
            .stroke(egui::Stroke::new(0.8, accent))
            .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0).color(accent));
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(label).strong().color(accent));
                });
                // While recording, show the live partial transcription growing
                // beneath the spinner as a muted, wrapped second line so the
                // user literally watches the sentence form before they release
                // the key. Display-only — the Final card still comes from the
                // utterance event.
                if stage == "recording" {
                    if let Some(preview) = self
                        .pipeline_preview
                        .as_deref()
                        .map(str::trim)
                        .filter(|preview| !preview.is_empty())
                    {
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(preview)
                                    .italics()
                                    .color(palette.text_muted),
                            )
                            .wrap(),
                        );
                    }
                }
            });
        ui.add_space(8.0);
    }

    fn render_log_cards(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let cards = runtime_log_cards(&self.runtime_log, self.runtime_log_view);
        if cards.is_empty() {
            empty_log_state(
                ui,
                self.display_runtime_state(),
                palette,
                &self.settings.ui_language,
            );
            return;
        }
        let dictation_badge = ui_text(&self.settings.ui_language, UiTextKey::Dictation).to_owned();
        let health_ok_badge = ui_text(&self.settings.ui_language, UiTextKey::HealthOk).to_owned();
        let health_warn_badge =
            ui_text(&self.settings.ui_language, UiTextKey::HealthWarn).to_owned();
        for mut card in cards {
            if card.title.trim().is_empty() {
                continue;
            }
            // Translate internal marker strings to user-visible localized badges
            // at render time so the log-parsing layer stays language-agnostic
            // and tests remain stable.
            if card.badge == "Utterance" {
                card.badge = dictation_badge.clone();
            } else if card.badge == "HealthOk" {
                card.badge = health_ok_badge.clone();
            } else if card.badge == "HealthWarn" {
                card.badge = health_warn_badge.clone();
            }
            runtime_log_card(ui, &card, palette);
            ui.add_space(8.0);
        }
    }

    fn listening_gauge(&self, ui: &mut egui::Ui, palette: UiPalette, max_width: f32) {
        let active = self.audio_capture_active && self.runtime_state == RuntimeState::Running;
        if active {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        let level = audio_meter_level(self.audio_meter_level, self.runtime_state, active);
        let status = if active {
            "Recording"
        } else if self.audio_capture_opening {
            "Opening"
        } else if self.runtime_state == RuntimeState::Running {
            if self.worker_ready {
                "Ready"
            } else {
                "Starting"
            }
        } else {
            "Idle"
        };
        let gauge_width = (max_width * 0.42).clamp(MIC_GAUGE_MIN_WIDTH, MIC_GAUGE_MAX_WIDTH);
        let label_width = (max_width - gauge_width - 8.0).max(0.0);
        let label_chars = mic_label_char_budget(label_width);
        let audio_device = audio_device_label(&self.active_audio_device, label_chars);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let response = level_gauge(ui, palette, level, active, gauge_width);
            ui.add_sized(
                egui::vec2(label_width, 18.0),
                egui::Label::new(
                    icon_text(
                        icons::ICON_MIC.codepoint,
                        format!("{status} - {audio_device}"),
                    )
                    .size(12.0)
                    .color(if active {
                        palette.accent_blue
                    } else {
                        palette.text_muted
                    }),
                ),
            );
            response.on_hover_text(format!(
                "Audio input: {}\nLive: {}\nCapture: {}\nGate: {}",
                full_audio_device_label(&self.active_audio_device),
                live_audio_level_summary(self.audio_meter_raw_dbfs, self.audio_meter_peak, active,),
                latest_metric_summary(&self.runtime_log, "[cap]"),
                latest_metric_summary(&self.runtime_log, "[gate]")
            ));
        });
    }

    pub(in crate::ui) fn session_panel(&self, ui: &mut egui::Ui, palette: UiPalette) {
        panel_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                icon_text(
                    icons::ICON_TASK_ALT.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::Session),
                )
                .strong(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                metric_box(ui, "Backend", self.backend_summary(), palette)
                    .on_hover_text("The active speech-to-text engine for this session.");
                metric_box(
                    ui,
                    "Post",
                    empty_as_disabled(&self.settings.post_processor),
                    palette,
                )
                .on_hover_text("The active post-processing provider (see the Post tab).");
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                metric_box(
                    ui,
                    "STT",
                    latest_metric_summary(&self.runtime_log, "[stt]"),
                    palette,
                )
                .on_hover_text(
                    "Last dictation: dur = how long you spoke, \
                     compute = transcription time, \
                     rtf = compute/duration (below 1.0 means faster than real time).",
                );
                metric_box(
                    ui,
                    "Inject",
                    latest_log_summary(&self.runtime_log, "[inject] strategy:"),
                    palette,
                )
                .on_hover_text(
                    "How the text was inserted last: \
                     type = simulated keystrokes, \
                     paste = clipboard + Ctrl+V. \
                     Auto picks per target window.",
                );
            });
        });
    }

    pub(in crate::ui) fn log_mode_selector(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for mode in LogViewMode::ALL {
                let selected = self.runtime_log_view == mode;
                let fill = if selected {
                    palette.accent_dark
                } else {
                    palette.surface_bg
                };
                let text = if selected {
                    egui::RichText::new(mode.label(&self.settings.ui_language))
                        .strong()
                        .color(palette.text)
                } else {
                    egui::RichText::new(mode.label(&self.settings.ui_language))
                        .color(palette.text_muted)
                };
                if ui
                    .add_sized(
                        egui::vec2(92.0, 30.0),
                        egui::Button::new(text)
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, palette.border_soft)),
                    )
                    .clicked()
                {
                    self.set_log_view(mode);
                }
            }
        });
    }

    fn visible_runtime_log(&self) -> String {
        log_view_text(&self.runtime_log, self.runtime_log_view)
    }

    pub(in crate::ui) fn backend_summary(&self) -> &str {
        match self.settings.stt_backend.as_str() {
            "parakeet" => "Parakeet",
            "openai" => self.current_cloud_provider().label(),
            _ => "Whisper",
        }
    }

    pub(in crate::ui) fn stt_detail_summary(&self) -> (&'static str, &'static str, String) {
        match SttBackendMode::from_raw(&self.settings.stt_backend) {
            SttBackendMode::Cloud => (
                ui_text(&self.settings.ui_language, UiTextKey::Model),
                icons::ICON_MODEL_TRAINING.codepoint,
                compact_label(self.cloud_stt_model_summary(), 28),
            ),
            SttBackendMode::Whisper | SttBackendMode::Parakeet => (
                ui_text(&self.settings.ui_language, UiTextKey::Compute),
                icons::ICON_MEMORY.codepoint,
                self.compute_summary(),
            ),
        }
    }

    fn cloud_stt_model_summary(&self) -> &str {
        let model = self.settings.stt_model.trim();
        if model.is_empty() {
            self.current_cloud_provider().default_model()
        } else {
            model
        }
    }

    fn compute_summary(&self) -> String {
        format!(
            "{} / {}",
            empty_as_auto(&self.settings.device),
            empty_as_auto(&self.settings.compute_type)
        )
    }
}

fn runtime_log_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
        .inner_margin(egui::Margin {
            // egui 0.34: `Margin` fields are `i8` (were `f32`).
            left: 12,
            right: 12,
            top: RUNTIME_LOG_TOP_MARGIN as i8,
            bottom: 10,
        })
}

pub(in crate::ui) fn level_gauge(
    ui: &mut egui::Ui,
    palette: UiPalette,
    level: f32,
    active: bool,
    width: f32,
) -> egui::Response {
    let size = egui::vec2(width.clamp(MIC_GAUGE_MIN_WIDTH, MIC_GAUGE_MAX_WIDTH), 18.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter_at(rect);
    // egui 0.34: `Painter::rect` gained a 5th `StrokeKind` argument.
    // `StrokeKind::Inside` reproduces the previous 4-arg behaviour (stroke drawn
    // inside the rect edge).
    painter.rect(
        rect,
        8.0,
        palette.header_bg,
        egui::Stroke::new(0.8, palette.border_soft),
        egui::StrokeKind::Inside,
    );

    let segments = 18;
    let gap = 2.0;
    let segment_width = (rect.width() - gap * (segments - 1) as f32) / segments as f32;
    let shown_level = if active { level.clamp(0.0, 1.0) } else { 0.0 };

    for index in 0..segments {
        let start_x = rect.left() + index as f32 * (segment_width + gap);
        let segment_rect = egui::Rect::from_min_size(
            egui::pos2(start_x, rect.top() + 3.0),
            egui::vec2(segment_width, rect.height() - 6.0),
        );
        let threshold = (index + 1) as f32 / segments as f32;
        let filled = shown_level >= threshold;
        let color = if filled {
            gauge_color_for_position(index as f32 / (segments - 1) as f32, palette)
        } else {
            palette.border_soft
        };
        painter.rect_filled(segment_rect, 3.0, color);
    }

    response
}

fn gauge_color_for_position(position: f32, palette: UiPalette) -> egui::Color32 {
    if position < 0.68 {
        palette.ok_text
    } else if position < 0.86 {
        palette.warn_text
    } else {
        palette.error_text
    }
}

fn latest_metric_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(compact_diagnostic_title)
        .unwrap_or_else(|| "No data yet".to_owned())
}

fn latest_log_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(strip_log_prefix)
        .unwrap_or("No data yet")
        .to_owned()
}

pub(in crate::ui) fn live_audio_level_summary(
    raw_dbfs: Option<f32>,
    peak: Option<f32>,
    active: bool,
) -> String {
    if !active {
        return "Not recording".to_owned();
    }
    match (raw_dbfs, peak) {
        (Some(raw_dbfs), Some(peak)) => format!("raw={raw_dbfs:.1}dBFS  peak={peak:.3}"),
        (Some(raw_dbfs), None) => format!("raw={raw_dbfs:.1}dBFS"),
        _ => "Waiting for audio level".to_owned(),
    }
}

pub(in crate::ui) fn mic_label_char_budget(width: f32) -> usize {
    ((width / 7.0).floor() as usize).clamp(8, 34)
}

pub(in crate::ui) fn audio_device_label(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Input pending".to_owned();
    }
    compact_label(value, max_chars.clamp(8, 34))
}

pub(in crate::ui) fn full_audio_device_label(value: &str) -> &str {
    let value = value.trim();
    if value.is_empty() {
        "Not reported yet"
    } else {
        value
    }
}

pub(in crate::ui) fn empty_as_auto(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Auto"
    } else {
        trimmed
    }
}

pub(in crate::ui) fn empty_as_disabled(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "none" {
        "Disabled"
    } else {
        trimmed
    }
}

fn runtime_status_badge(
    ui: &mut egui::Ui,
    state: RuntimeState,
    palette: UiPalette,
    raw_language: &str,
) {
    let (fill, stroke, text) = match state {
        RuntimeState::Stopped => (palette.surface_bg, palette.border, palette.text_muted),
        RuntimeState::Starting => (
            palette.accent_dark,
            palette.accent_blue,
            palette.accent_blue,
        ),
        RuntimeState::Running => (palette.surface_active_bg, palette.ok_text, palette.ok_text),
    };
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(0.8, stroke))
        .corner_radius(egui::CornerRadius::same(PILL_RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    ui_text(raw_language, UiTextKey::Status),
                    runtime_state_label(state, raw_language)
                ))
                .strong()
                .color(text),
            );
        });
}

/// A pill that shows the currently configured hotkey/chord so it is always
/// visible while watching live dictation. The raw setting (e.g. `ctrl_r` or
/// `shift_l+ctrl_l`) is rendered as a human-friendly chord (`Ctrl (right)`).
/// When toggle mode is on the label reads "Toggle key" (press to start, press
/// again to stop); otherwise the usual "Push-to-talk".
/// Mode-labelled chord text — "Push-to-talk: <chord>" (hold mode) or
/// "Toggle key: <chord>" (toggle mode). Used for the sidebar key display's
/// hover text; kept pure so it is unit-testable without an egui context.
pub(in crate::ui) fn push_to_talk_badge_label(
    raw_keys: &str,
    toggle_mode: bool,
    raw_language: &str,
) -> String {
    let prefix = if toggle_mode {
        ui_text(raw_language, UiTextKey::Toggle)
    } else {
        ui_text(raw_language, UiTextKey::PushToTalk)
    };
    format!("{}: {}", prefix, format_push_to_talk_keys(raw_keys))
}

/// Render a raw hotkey setting (`ctrl_r`, `shift_l+ctrl_l`, …) as a friendly
/// chord. Empty input becomes `None`; unknown tokens are passed through
/// capitalized so custom keys still read sensibly.
pub(in crate::ui) fn format_push_to_talk_keys(raw: &str) -> String {
    let chord = raw
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(format_key_token)
        .collect::<Vec<_>>();
    if chord.is_empty() {
        return "None".to_owned();
    }
    chord.join(" + ")
}

fn format_key_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    let (base, side) = if let Some(base) = lower
        .strip_suffix("_l")
        .or_else(|| lower.strip_suffix("_left"))
    {
        (base, Some("left"))
    } else if let Some(base) = lower
        .strip_suffix("_r")
        .or_else(|| lower.strip_suffix("_right"))
    {
        (base, Some("right"))
    } else {
        (lower.as_str(), None)
    };
    let label = match base {
        "ctrl" | "control" => "Ctrl".to_owned(),
        "shift" => "Shift".to_owned(),
        "alt" | "option" => "Alt".to_owned(),
        "cmd" | "command" | "super" | "win" | "meta" => "Cmd/Win".to_owned(),
        "space" => "Space".to_owned(),
        other => capitalize_ascii(other),
    };
    match side {
        Some(side) => format!("{label} ({side})"),
        None => label,
    }
}

fn capitalize_ascii(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => token.to_owned(),
    }
}

/// Accent colour for the live pipeline-progress card.
///
/// The card is red while the microphone is actively capturing audio
/// ("recording") so the user gets immediate visual feedback that their key
/// press is live. Once the audio has been handed to the model the card turns
/// the calmer warn/blue colours used for the processing stages.
pub(in crate::ui) fn pipeline_progress_accent_color(
    stage: &str,
    palette: UiPalette,
) -> egui::Color32 {
    match stage {
        "recording" => palette.error_text,
        "transcribing" => palette.warn_text,
        _ => palette.accent_blue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ui_palette;

    #[test]
    fn pipeline_progress_accent_recording_is_red() {
        let dark = ui_palette("dark");
        let light = ui_palette("light");
        // "recording" must use the error/red colour in both themes.
        assert_eq!(
            pipeline_progress_accent_color("recording", dark),
            dark.error_text
        );
        assert_eq!(
            pipeline_progress_accent_color("recording", light),
            light.error_text
        );
        // Red must differ from the ok/green colour so the visual distinction is real.
        assert_ne!(dark.error_text, dark.ok_text);
        assert_ne!(light.error_text, light.ok_text);
    }

    #[test]
    fn pipeline_progress_accent_transcribing_is_warn() {
        let palette = ui_palette("dark");
        assert_eq!(
            pipeline_progress_accent_color("transcribing", palette),
            palette.warn_text
        );
    }

    #[test]
    fn pipeline_progress_accent_post_processing_is_blue() {
        let palette = ui_palette("dark");
        assert_eq!(
            pipeline_progress_accent_color("post-processing", palette),
            palette.accent_blue
        );
    }

    #[test]
    fn pipeline_progress_accent_unknown_stage_falls_back_to_blue() {
        let palette = ui_palette("dark");
        assert_eq!(
            pipeline_progress_accent_color("unknown-stage", palette),
            palette.accent_blue
        );
    }

    #[test]
    fn pipeline_progress_accent_recording_differs_from_finished_card_color() {
        // The finished card (FinalText kind) uses ok_text/green; the in-progress
        // recording card must use a visually distinct red.
        let dark = ui_palette("dark");
        let light = ui_palette("light");
        assert_ne!(
            pipeline_progress_accent_color("recording", dark),
            dark.ok_text,
            "dark: recording accent must not be the same green used for finished cards"
        );
        assert_ne!(
            pipeline_progress_accent_color("recording", light),
            light.ok_text,
            "light: recording accent must not be the same green used for finished cards"
        );
    }

    /// Every stage string produced by `pipeline_stage_for_worker_state` must map
    /// to its expected NON-fallback colour.  This guards against a future rename
    /// (e.g. "post_processing" instead of "post-processing") silently falling
    /// through to the `_ => accent_blue` default without anyone noticing.
    #[test]
    fn pipeline_stage_for_worker_state_produces_non_fallback_accent_colors() {
        let palette = ui_palette("dark");
        let cases = [
            ("recording", palette.error_text),
            ("transcribing", palette.warn_text),
            ("post-processing", palette.accent_blue),
        ];
        for (worker_state, expected_accent) in cases {
            let stage = pipeline_stage_for_worker_state(worker_state)
                .unwrap_or_else(|| panic!("worker state {worker_state:?} produced no stage"));
            let actual = pipeline_progress_accent_color(stage, palette);
            assert_eq!(
                actual, expected_accent,
                "worker state {worker_state:?} → stage {stage:?}: expected accent \
                 {expected_accent:?}, got {actual:?}"
            );
            // Guard: the accent must not be the blue fallback for recording/transcribing
            // (those have distinct colours; a typo would silently land on blue).
            if worker_state != "post-processing" {
                assert_ne!(
                    actual, palette.accent_blue,
                    "worker state {worker_state:?} must not fall back to accent_blue"
                );
            }
        }
    }
}
