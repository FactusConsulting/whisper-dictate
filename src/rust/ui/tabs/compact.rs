//! Compact mode: a tiny, always-on-top strip that keeps Start/Stop and the
//! microphone level visible while the user dictates into another application.
//!
//! Compact mode is **session-only UI state** (the `compact_mode` flag on
//! `WhisperDictateApp`), never persisted to the config. Entering/leaving it only
//! resizes and re-levels the existing viewport — the Python dictation worker keeps
//! running across the switch, so `update()` runs the runtime/background polls
//! before it branches into the compact layout.

use super::status_surface::{compact_status_color, compact_status_label, compact_status_state};
use super::*;
use egui_material_icons::icons;

/// Compact surface target inner size (logical points). The extra height keeps
/// the retained transcript and its action row visible below live progress.
pub(in crate::ui) const COMPACT_INNER_SIZE: [f32; 2] = [560.0, 230.0];
/// Compact surface minimum size — keeps controls, progress, and transcript
/// actions visible when the user resizes the window.
pub(in crate::ui) const COMPACT_MIN_INNER_SIZE: [f32; 2] = [460.0, 210.0];
/// Full-window inner size restored when leaving compact mode (matches `run()`).
pub(in crate::ui) const FULL_INNER_SIZE: [f32; 2] = [1080.0, 760.0];
/// Full-window minimum inner size restored when leaving compact mode (matches the
/// floor in `run()` that stops the top status bar from being squeezed).
pub(in crate::ui) const FULL_MIN_INNER_SIZE: [f32; 2] = [1000.0, 640.0];

const COMPACT_STATUS_WIDTH: f32 = 128.0;

/// Width budget for the mic level gauge + device label inside the compact strip.
const COMPACT_MIC_WIDTH: f32 = 150.0;
/// Minimum characters of the active device name to keep legible even when the
/// compact strip is dragged to its narrowest. Above this the budget grows with
/// the actually-available label width so widening the window reveals the full
/// device name.
const COMPACT_DEVICE_LABEL_MIN_CHARS: usize = 8;
/// Characters of live preview text shown on the optional progress line.
const COMPACT_PREVIEW_CHARS: usize = 60;

/// The exact viewport commands to send when toggling compact mode, returned as
/// data so the mode-switch behaviour is unit-testable without a live viewport.
///
/// `enter == true` shrinks the window, drops the minimum so the strip can be
/// small, raises it to always-on-top, and keeps native decorations so the user
/// can still drag/close it via the titlebar. `enter == false` restores the full
/// window geometry and normal window level.
///
/// Order matters: lower the `MinInnerSize` floor *before* the `InnerSize` when
/// entering (so the new small size isn't clamped up by the old large minimum),
/// and raise the `MinInnerSize` floor *after* the `InnerSize` when leaving (so the
/// large size isn't clamped down by the old small minimum mid-resize).
pub(in crate::ui) fn compact_toggle_viewport_cmds(
    enter: bool,
    raw_scale: &str,
) -> Vec<egui::ViewportCommand> {
    let scale = layout_scale(raw_scale);
    let compact_size = egui::vec2(COMPACT_INNER_SIZE[0] * scale, COMPACT_INNER_SIZE[1] * scale);
    let compact_min_size = egui::vec2(
        COMPACT_MIN_INNER_SIZE[0] * scale,
        COMPACT_MIN_INNER_SIZE[1] * scale,
    );
    if enter {
        vec![
            egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop),
            egui::ViewportCommand::Decorations(true),
            egui::ViewportCommand::MinInnerSize(compact_min_size),
            egui::ViewportCommand::InnerSize(compact_size),
        ]
    } else {
        vec![
            egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal),
            egui::ViewportCommand::Decorations(true),
            egui::ViewportCommand::InnerSize(FULL_INNER_SIZE.into()),
            egui::ViewportCommand::MinInnerSize(FULL_MIN_INNER_SIZE.into()),
        ]
    }
}

impl WhisperDictateApp {
    /// Toggle compact mode and send the corresponding viewport commands. Pure
    /// state change + a fixed command list (see `compact_toggle_viewport_cmds`);
    /// it never touches the worker so dictation continues across the switch.
    pub(in crate::ui) fn set_compact_mode(&mut self, ctx: &egui::Context, compact: bool) {
        if self.compact_mode == compact {
            return;
        }
        self.compact_mode = compact;
        for cmd in compact_toggle_viewport_cmds(compact, &self.settings.ui_text_scale) {
            ctx.send_viewport_cmd(cmd);
        }
    }

    /// The floating status surface: lifecycle controls, model/profile metadata,
    /// microphone level, the latest transcript, and quick actions.
    pub(in crate::ui) fn compact_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let status = compact_status_state(
            self.runtime_state,
            self.worker_ready,
            self.pipeline_stage,
            self.last_runtime_error.is_some(),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            // Status dot: same colour mapping as the top status bar.
            let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 6.0, compact_status_color(status, palette));

            self.compact_start_stop(ui, palette);
            self.compact_mic(ui, palette);
            compact_status_label(ui, status, palette, &self.settings.ui_language);

            // Exit-compact button, pinned to the right edge.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(
                        egui::RichText::new(icons::ICON_OPEN_IN_FULL.codepoint).color(palette.text),
                    ))
                    .on_hover_text(ui_text(&self.settings.ui_language, UiTextKey::LeaveCompact))
                    .clicked()
                {
                    self.set_compact_mode(ui.ctx(), false);
                }
            });
        });
        self.compact_metadata(ui, palette);
        self.compact_progress(ui, palette);
        self.last_transcript_panel(ui, palette);
    }

    /// Start/Stop in the compact strip — reuses the exact same lifecycle calls as
    /// `global_controls`, so the worker behaves identically in either layout.
    fn compact_start_stop(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let is_stopped = self.runtime_state == RuntimeState::Stopped;
        if is_stopped {
            if ui
                .add(
                    egui::Button::new(
                        icon_text(
                            icons::ICON_PLAY_ARROW.codepoint,
                            ui_text(&self.settings.ui_language, UiTextKey::Start),
                        )
                        .strong(),
                    )
                    .fill(palette.accent_dark)
                    .min_size(egui::vec2(80.0, 30.0)),
                )
                .clicked()
            {
                self.start_runtime();
            }
        } else if ui
            .add(
                egui::Button::new(
                    icon_text(
                        icons::ICON_STOP.codepoint,
                        ui_text(&self.settings.ui_language, UiTextKey::Stop),
                    )
                    .strong(),
                )
                .fill(palette.error_text)
                .min_size(egui::vec2(80.0, 30.0)),
            )
            .clicked()
        {
            self.stop_runtime();
        }
    }

    /// Mic level gauge + a short status/device label, reusing the same
    /// `level_gauge` widget as the full runtime tab.
    fn compact_mic(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let active = self.audio_capture_active && self.runtime_state == RuntimeState::Running;
        if active {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
        let level = audio_meter_level(self.audio_meter_level, self.runtime_state, active);
        let gauge_width = (COMPACT_MIC_WIDTH * 0.5).clamp(70.0, 100.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        // The device label fills whatever space remains on the row after the
        // gauge and the right-pinned exit button, minus the inter-item gaps. So
        // widening the compact window grows the visible device name instead of
        // truncating it at a fixed width.
        let exit_button_width = 34.0;
        let label_width =
            (ui.available_width() - gauge_width - exit_button_width - COMPACT_STATUS_WIDTH - 26.0)
                .max(0.0);
        let device_chars = compact_mic_label_char_budget(label_width);
        level_gauge(ui, palette, level, active, gauge_width).on_hover_text(format!(
            "Audio input: {}\nLive: {}",
            full_audio_device_label(&self.active_audio_device),
            live_audio_level_summary(self.audio_meter_raw_dbfs, self.audio_meter_peak, active),
        ));
        let device = audio_device_label(&self.active_audio_device, device_chars);
        ui.label(
            icon_text(icons::ICON_MIC.codepoint, device)
                .size(12.0)
                .color(if active {
                    palette.accent_blue
                } else {
                    palette.text_muted
                }),
        );
    }

    /// One-line dictation progress (spinner + stage + truncated preview) so the
    /// user can see the pipeline working from the tiny strip. Hidden when idle.
    fn compact_progress(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let Some((label, accent)) =
            compact_stage_label(self.pipeline_stage, palette, &self.settings.ui_language)
        else {
            return;
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(egui::Spinner::new().size(13.0).color(accent));
            ui.label(egui::RichText::new(label).strong().color(accent));
            if let Some(preview) = self
                .pipeline_preview
                .as_deref()
                .map(str::trim)
                .filter(|preview| !preview.is_empty())
            {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(compact_label(preview, COMPACT_PREVIEW_CHARS))
                            .italics()
                            .color(palette.text_muted),
                    )
                    .truncate(),
                )
                .on_hover_text(preview);
            }
        });
    }
}

/// Characters of the device name the compact strip can show given the pixel
/// width left for the label after the gauge + exit button. Derives the budget
/// from the actually-available width (≈7px per glyph, matching the full runtime
/// tab's `mic_label_char_budget`) so widening the compact window reveals the
/// full device name; clamped to a legible minimum and the 34-char ceiling that
/// `audio_device_label` itself enforces. Pure for unit testing.
pub(in crate::ui) fn compact_mic_label_char_budget(width: f32) -> usize {
    ((width / 7.0).floor() as usize).clamp(COMPACT_DEVICE_LABEL_MIN_CHARS, 34)
}

/// Map a pipeline stage to its compact label + accent colour, or `None` when no
/// utterance is in flight. Pure so the stage→label mapping is unit-testable.
pub(in crate::ui) fn compact_stage_label(
    stage: Option<&'static str>,
    palette: UiPalette,
    language: &str,
) -> Option<(&'static str, egui::Color32)> {
    let stage = stage?;
    let label = match stage {
        "recording" => ui_text(language, UiTextKey::CompactRecordingProgress),
        "transcribing" => ui_text(language, UiTextKey::CompactTranscribing),
        "post-processing" => ui_text(language, UiTextKey::CompactPostProcessing),
        "injecting" => ui_text(language, UiTextKey::CompactInjecting),
        _ => return None,
    };
    Some((label, pipeline_progress_accent_color(stage, palette)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_is_window_level(cmd: &egui::ViewportCommand, expected: egui::WindowLevel) -> bool {
        matches!(cmd, egui::ViewportCommand::WindowLevel(level) if *level == expected)
    }

    #[test]
    fn entering_compact_raises_always_on_top_and_shrinks_after_lowering_min() {
        let cmds = compact_toggle_viewport_cmds(true, "1.0");
        assert!(cmd_is_window_level(
            &cmds[0],
            egui::WindowLevel::AlwaysOnTop
        ));
        // Native decorations stay so the user can drag/close via the titlebar.
        assert!(matches!(cmds[1], egui::ViewportCommand::Decorations(true)));
        // The min floor must drop before the inner size shrinks, otherwise the
        // small size is clamped back up by the old 1000x640 minimum.
        let min_idx = cmds
            .iter()
            .position(|c| matches!(c, egui::ViewportCommand::MinInnerSize(_)))
            .expect("min inner size command");
        let inner_idx = cmds
            .iter()
            .position(|c| matches!(c, egui::ViewportCommand::InnerSize(_)))
            .expect("inner size command");
        assert!(min_idx < inner_idx, "min must be lowered before resize");
        assert!(matches!(
            cmds[min_idx],
            egui::ViewportCommand::MinInnerSize(v) if v == egui::Vec2::from(COMPACT_MIN_INNER_SIZE)
        ));
        assert!(matches!(
            cmds[inner_idx],
            egui::ViewportCommand::InnerSize(v) if v == egui::Vec2::from(COMPACT_INNER_SIZE)
        ));
    }

    #[test]
    fn compact_viewport_grows_with_large_text_scale() {
        let cmds = compact_toggle_viewport_cmds(true, "1.6");
        assert!(matches!(
            cmds[2],
            egui::ViewportCommand::MinInnerSize(v)
                if v == egui::vec2(460.0 * 1.6, 210.0 * 1.6)
        ));
        assert!(matches!(
            cmds[3],
            egui::ViewportCommand::InnerSize(v)
                if v == egui::vec2(560.0 * 1.6, 230.0 * 1.6)
        ));
    }

    #[test]
    fn leaving_compact_restores_full_window_and_normal_level() {
        let cmds = compact_toggle_viewport_cmds(false, "1.0");
        assert!(cmd_is_window_level(&cmds[0], egui::WindowLevel::Normal));
        // Restore the large size before re-raising the min floor, otherwise the
        // big size is clamped down by the old small minimum.
        let inner_idx = cmds
            .iter()
            .position(|c| matches!(c, egui::ViewportCommand::InnerSize(_)))
            .expect("inner size command");
        let min_idx = cmds
            .iter()
            .position(|c| matches!(c, egui::ViewportCommand::MinInnerSize(_)))
            .expect("min inner size command");
        assert!(
            inner_idx < min_idx,
            "resize must precede re-raising the min"
        );
        assert!(matches!(
            cmds[inner_idx],
            egui::ViewportCommand::InnerSize(v) if v == egui::Vec2::from(FULL_INNER_SIZE)
        ));
        assert!(matches!(
            cmds[min_idx],
            egui::ViewportCommand::MinInnerSize(v) if v == egui::Vec2::from(FULL_MIN_INNER_SIZE)
        ));
    }

    #[test]
    fn full_min_inner_size_matches_run_window_floor() {
        // The restored floor must equal the launch floor in `run()` so leaving
        // compact mode lands the user back at the exact window they started with.
        assert_eq!(FULL_MIN_INNER_SIZE, [1000.0, 640.0]);
        assert_eq!(FULL_INNER_SIZE, [1080.0, 760.0]);
    }

    #[test]
    fn compact_stage_label_maps_known_stages_and_ignores_idle() {
        let palette = ui_palette("dark");
        assert!(compact_stage_label(None, palette, "en").is_none());
        assert!(compact_stage_label(Some("unknown"), palette, "en").is_none());
        assert_eq!(
            compact_stage_label(Some("recording"), palette, "en").map(|(l, _)| l),
            Some("Recording…")
        );
        assert_eq!(
            compact_stage_label(Some("transcribing"), palette, "en").map(|(l, _)| l),
            Some("Transcribing…")
        );
        assert_eq!(
            compact_stage_label(Some("post-processing"), palette, "en").map(|(l, _)| l),
            Some("Post-processing…")
        );
        assert_eq!(
            compact_stage_label(Some("injecting"), palette, "en").map(|(l, _)| l),
            Some("Injecting…")
        );
    }

    #[test]
    fn compact_mic_label_budget_grows_with_width_and_keeps_a_minimum() {
        // Narrow / zero space falls back to the legible minimum, not 0.
        assert_eq!(compact_mic_label_char_budget(0.0), 8);
        assert_eq!(compact_mic_label_char_budget(40.0), 8);
        // A wider strip reveals more of the device name (≈7px/glyph).
        assert_eq!(compact_mic_label_char_budget(140.0), 20);
        // Very wide is capped at the 34-char ceiling audio_device_label enforces,
        // which is enough for full names like "Microphone (Yeti Classic)".
        assert_eq!(compact_mic_label_char_budget(1000.0), 34);
        // The wide budget must comfortably fit a typical full device name so
        // widening actually shows it untruncated.
        let full = "Microphone (Yeti Classic)";
        assert_eq!(
            audio_device_label(full, compact_mic_label_char_budget(400.0)),
            full
        );
        // While a narrow strip truncates the same name.
        assert!(audio_device_label(full, compact_mic_label_char_budget(60.0)).ends_with("..."));
    }

    #[test]
    fn compact_stage_label_recording_accent_is_red() {
        // The compact strip uses the same accent-colour logic as the full log
        // card: red while recording, calmer colours once the audio is gone.
        let palette = ui_palette("dark");
        let (_, recording_color) = compact_stage_label(Some("recording"), palette, "en").unwrap();
        assert_eq!(
            recording_color, palette.error_text,
            "recording accent must be red (error_text)"
        );
        // The transcribing and post-processing stages must NOT be red.
        let (_, transcribing_color) =
            compact_stage_label(Some("transcribing"), palette, "en").unwrap();
        assert_ne!(transcribing_color, palette.error_text);
    }
}
