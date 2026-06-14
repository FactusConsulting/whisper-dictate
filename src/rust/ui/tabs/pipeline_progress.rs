use super::super::*;
use super::*;

impl WhisperDictateApp {
    /// Live card for the in-flight utterance: spins through the pipeline stages
    /// (recording → transcribing → post-processing) so slow CPU runs show
    /// progress. Cleared once the utterance settles into its Final card.
    pub(in crate::ui) fn render_pipeline_progress(&self, ui: &mut egui::Ui, palette: UiPalette) {
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
