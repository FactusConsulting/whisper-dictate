//! Compact mode: a tiny, always-on-top strip that keeps Start/Stop and the
//! microphone level visible while the user dictates into another application.
//!
//! Compact mode is **session-only UI state** (the `compact_mode` flag on
//! `WhisperDictateApp`), never persisted to the config. Entering/leaving it only
//! resizes and re-levels the existing viewport — the Python dictation worker keeps
//! running across the switch, so `update()` runs the runtime/background polls
//! before it branches into the compact layout.

use super::super::*;
use super::*;
use egui_material_icons::icons;

/// Compact strip target inner size (logical points). Wide enough for the status
/// dot, Start/Stop, a short mic gauge + device label, and the exit button on one
/// row, short enough to hug a screen edge.
pub(in crate::ui) const COMPACT_INNER_SIZE: [f32; 2] = [420.0, 96.0];
/// Compact strip minimum inner size — keeps the single control row legible if the
/// user drags the window smaller.
pub(in crate::ui) const COMPACT_MIN_INNER_SIZE: [f32; 2] = [360.0, 92.0];
/// Full-window inner size restored when leaving compact mode (matches `run()`).
pub(in crate::ui) const FULL_INNER_SIZE: [f32; 2] = [1080.0, 760.0];
/// Full-window minimum inner size restored when leaving compact mode (matches the
/// floor in `run()` that stops the top status bar from being squeezed).
pub(in crate::ui) const FULL_MIN_INNER_SIZE: [f32; 2] = [1000.0, 640.0];

/// Width budget for the mic level gauge + device label inside the compact strip.
const COMPACT_MIC_WIDTH: f32 = 150.0;
/// Characters of the active device name shown beside the gauge in compact mode.
const COMPACT_DEVICE_LABEL_CHARS: usize = 16;
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
pub(in crate::ui) fn compact_toggle_viewport_cmds(enter: bool) -> Vec<egui::ViewportCommand> {
    if enter {
        vec![
            egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop),
            egui::ViewportCommand::Decorations(true),
            egui::ViewportCommand::MinInnerSize(COMPACT_MIN_INNER_SIZE.into()),
            egui::ViewportCommand::InnerSize(COMPACT_INNER_SIZE.into()),
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
        for cmd in compact_toggle_viewport_cmds(compact) {
            ctx.send_viewport_cmd(cmd);
        }
    }

    /// The whole-window compact layout: a single control row plus an optional
    /// one-line dictation-progress indicator. No sidebar, tabs, log, or message
    /// bar — just enough to drive dictation while it floats over other apps.
    pub(in crate::ui) fn compact_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let display_state = self.display_runtime_state();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            // Status dot: same colour mapping as the top status bar.
            let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(
                dot.center(),
                6.0,
                runtime_state_color(display_state, palette),
            );

            self.compact_start_stop(ui, palette);
            self.compact_mic(ui, palette);

            // Exit-compact button, pinned to the right edge.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(
                        egui::RichText::new(icons::ICON_OPEN_IN_FULL).color(palette.text),
                    ))
                    .on_hover_text("Leave compact mode")
                    .clicked()
                {
                    self.set_compact_mode(ui.ctx(), false);
                }
            });
        });
        self.compact_progress(ui, palette);
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
                            icons::ICON_PLAY_ARROW,
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
                        icons::ICON_STOP,
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
        level_gauge(ui, palette, level, active, gauge_width).on_hover_text(format!(
            "Audio input: {}\nLive: {}",
            full_audio_device_label(&self.active_audio_device),
            live_audio_level_summary(self.audio_meter_raw_dbfs, self.audio_meter_peak, active),
        ));
        let device = audio_device_label(&self.active_audio_device, COMPACT_DEVICE_LABEL_CHARS);
        ui.label(
            icon_text(icons::ICON_MIC, device)
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
        let Some((label, accent)) = compact_stage_label(self.pipeline_stage, palette) else {
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

/// Map a pipeline stage to its compact label + accent colour, or `None` when no
/// utterance is in flight. Pure so the stage→label mapping is unit-testable.
pub(in crate::ui) fn compact_stage_label(
    stage: Option<&'static str>,
    palette: UiPalette,
) -> Option<(&'static str, egui::Color32)> {
    match stage? {
        "recording" => Some((
            "Recording…",
            pipeline_progress_accent_color("recording", palette),
        )),
        "transcribing" => Some((
            "Transcribing…",
            pipeline_progress_accent_color("transcribing", palette),
        )),
        "post-processing" => Some((
            "Post-processing…",
            pipeline_progress_accent_color("post-processing", palette),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_is_window_level(cmd: &egui::ViewportCommand, expected: egui::WindowLevel) -> bool {
        matches!(cmd, egui::ViewportCommand::WindowLevel(level) if *level == expected)
    }

    #[test]
    fn entering_compact_raises_always_on_top_and_shrinks_after_lowering_min() {
        let cmds = compact_toggle_viewport_cmds(true);
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
    fn leaving_compact_restores_full_window_and_normal_level() {
        let cmds = compact_toggle_viewport_cmds(false);
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
        assert!(compact_stage_label(None, palette).is_none());
        assert!(compact_stage_label(Some("unknown"), palette).is_none());
        assert_eq!(
            compact_stage_label(Some("recording"), palette).map(|(l, _)| l),
            Some("Recording…")
        );
        assert_eq!(
            compact_stage_label(Some("transcribing"), palette).map(|(l, _)| l),
            Some("Transcribing…")
        );
        assert_eq!(
            compact_stage_label(Some("post-processing"), palette).map(|(l, _)| l),
            Some("Post-processing…")
        );
    }

    #[test]
    fn compact_stage_label_recording_accent_is_red() {
        // The compact strip uses the same accent-colour logic as the full log
        // card: red while recording, calmer colours once the audio is gone.
        let palette = ui_palette("dark");
        let (_, recording_color) = compact_stage_label(Some("recording"), palette).unwrap();
        assert_eq!(
            recording_color, palette.error_text,
            "recording accent must be red (error_text)"
        );
        // The transcribing and post-processing stages must NOT be red.
        let (_, transcribing_color) = compact_stage_label(Some("transcribing"), palette).unwrap();
        assert_ne!(transcribing_color, palette.error_text);
    }
}
