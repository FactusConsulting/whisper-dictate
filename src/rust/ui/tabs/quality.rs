use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn quality_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Quality");
        ui.add_space(6.0);
        let language = self.settings.ui_language.clone();

        // --- All backends: capture, normalization, and anti-hallucination gates
        // enforced by the native in-process session.
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::QualityGroupAllBackends),
            "quality_all_backends",
            |ui| {
                numeric_help(
                    ui,
                    &language,
                    "min_record_seconds",
                    "Min recording seconds",
                    &mut self.settings.min_record_seconds,
                    "Discard recordings shorter than this (seconds) as accidental key taps before transcription. Clamped to a 0.3 s floor so even 0 keeps misfire protection. Helps avoid hallucinated subtitle/caption credits on quiet taps.",
                );
                numeric_help(
                    ui,
                    &language,
                    "max_chars_per_second",
                    "Max chars per second",
                    &mut self.settings.max_chars_per_second,
                    "Drop a transcript whose characters-per-second is humanly impossible (real speech is ~15-25; default 30). Catches hallucinated subtitle/caption credits on quiet input. 0 disables this guard.",
                );
                numeric_help(
                    ui,
                    &language,
                    "release_tail_ms",
                    "Release tail ms",
                    &mut self.settings.release_tail_ms,
                    "Extra audio kept after releasing the hotkey so word endings are not clipped.",
                );
                numeric_help(
                    ui,
                    &language,
                    "max_record_s",
                    "Max recording seconds",
                    &mut self.settings.max_record_s,
                    "Maximum recording length in seconds. If a key is held down longer than this, further audio is silently dropped and a warning is logged. 0 disables the cap.",
                );
                text_help_short(
                    ui,
                    "Target dBFS",
                    &mut self.settings.target_dbfs,
                    "Audio normalization target loudness before transcription.",
                );
                text_help_short(
                    ui,
                    "Min input dBFS",
                    &mut self.settings.min_input_dbfs,
                    "Minimum raw microphone loudness accepted as speech candidate.",
                );
                text_help_short(
                    ui,
                    "Min SNR dB",
                    &mut self.settings.min_snr_db,
                    "Minimum signal-to-noise ratio accepted before transcription.",
                );
                checkbox_help(
                    ui,
                    "Audio ducking",
                    &mut self.settings.audio_ducking,
                    "Windows-only: temporarily lowers other app audio while recording, then restores it.",
                );
                numeric_help(
                    ui,
                    &language,
                    "audio_ducking_level",
                    "Audio ducking level",
                    &mut self.settings.audio_ducking_level,
                    "Target volume for other apps while recording. 0.25 means 25%.",
                );
            },
        );

        ui.add_space(10.0);

        // Native Whisper quality and guard settings.
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::QualityGroupWhisper),
            "quality_whisper",
            |ui| {
                numeric_help(
                    ui,
                    &language,
                    "preview_seconds",
                    "Live preview seconds",
                    &mut self.settings.preview_seconds,
                    "While recording, transcribe the buffer this often (seconds) so the live card shows the sentence growing. 0 disables. LOCAL Whisper backend only — ignored for cloud STT. The final result at key release is unchanged.",
                );
            },
        );

        // Wave 8 of #348 removed the Parakeet-specific quality group
        // ("Parakeet min seconds") together with the backend.

        ui.add_space(12.0);
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to native whisper.cpp for vocabulary and style hints. Keep it short; dictionary terms are capped separately, and the same prompt also feeds dictionary-term matching.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to native whisper.cpp for vocabulary and style hints. Keep it short; dictionary terms are capped separately, and the same prompt also feeds dictionary-term matching.");
        let initial_prompt_before = self.settings.initial_prompt.clone();
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        let initial_prompt_after = self.settings.initial_prompt.clone();
        self.record_nullable_text_edit(
            "initial_prompt",
            &initial_prompt_before,
            &initial_prompt_after,
        );
    }
}
