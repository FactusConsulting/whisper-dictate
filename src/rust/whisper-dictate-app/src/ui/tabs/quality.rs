use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn quality_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Quality");
        settings_grid("quality_settings")
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Beam size",
                    &mut self.settings.beam_size,
                    "Whisper beam search width. Higher can improve accuracy but costs more compute.",
                );
                text_help(
                    ui,
                    "Temperature ladder",
                    &mut self.settings.temperature,
                    "Comma-separated Whisper fallback temperatures, for example 0.0,0.2.",
                );
                text_help(
                    ui,
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                    "Minimum utterance length before passing previous context/prompt hints to Whisper.",
                );
                text_help(
                    ui,
                    "Parakeet min seconds",
                    &mut self.settings.parakeet_min_seconds,
                    "Minimum captured audio length before Parakeet transcription is attempted.",
                );
                text_help(
                    ui,
                    "Release tail ms",
                    &mut self.settings.release_tail_ms,
                    "Extra audio kept after releasing the hotkey so word endings are not clipped.",
                );
                text_help(
                    ui,
                    "VAD threshold",
                    &mut self.settings.vad_threshold,
                    "Voice activity detection sensitivity. Lower is more sensitive, higher rejects more noise.",
                );
                text_help(
                    ui,
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                    "Silence duration used by VAD to split or end speech.",
                );
                text_help(
                    ui,
                    "VAD speech pad ms",
                    &mut self.settings.vad_speech_pad_ms,
                    "Audio padding kept around detected speech so soft first and last syllables are not trimmed.",
                );
                text_help(
                    ui,
                    "Target dBFS",
                    &mut self.settings.target_dbfs,
                    "Audio normalization target loudness before transcription.",
                );
                text_help(
                    ui,
                    "Min input dBFS",
                    &mut self.settings.min_input_dbfs,
                    "Minimum raw microphone loudness accepted as speech candidate.",
                );
                text_help(
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
                text_help(
                    ui,
                    "Audio ducking level",
                    &mut self.settings.audio_ducking_level,
                    "Target volume for other apps while recording. 0.25 means 25%.",
                );
            });
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }
}
