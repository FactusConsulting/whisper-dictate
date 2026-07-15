use super::super::*;
use super::*;

impl WhisperDictateApp {
    pub(in crate::ui) fn quality_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Quality");
        ui.add_space(6.0);
        // Engine-specific decode settings are greyed out unless their engine is
        // the active STT backend, so it's clear which knobs actually take effect.
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);
        let whisper = backend == SttBackendMode::Whisper;
        let language = self.settings.ui_language.clone();

        // --- All backends: capture/normalization/anti-hallucination gates that
        // run on every STT engine. min_record_seconds is enforced in the worker's
        // _should_skip_pcm (pre-transcription, backend-independent); release_tail
        // and max recording seconds live in audio capture; max_chars_per_second
        // and the dBFS/SNR gates run in the unified _transcribe_detail path that
        // every backend's model.transcribe() goes through.
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

        // --- Whisper: faster-whisper decode knobs. beam_size, temperature,
        // context_min_seconds and the hallucination guard are passed straight to
        // faster-whisper's model.transcribe(); the VAD threshold/min-silence/
        // speech-pad feed its vad_parameters. Live preview is gated to the local
        // whisper backend (vp_preview.PREVIEW_BACKENDS) so it never hits a paid
        // cloud API.
        scope_group(
            ui,
            palette,
            ui_text(&language, UiTextKey::QualityGroupWhisper),
            "quality_whisper",
            |ui| {
                numeric_enabled(
                    ui,
                    &language,
                    whisper,
                    "beam_size",
                    "Beam size",
                    &mut self.settings.beam_size,
                    "Whisper beam search width. Higher can improve accuracy but costs more compute. Used only with STT backend = whisper.",
                );
                text_enabled_short(
                    ui,
                    whisper,
                    "Temperature ladder",
                    &mut self.settings.temperature,
                    "Comma-separated Whisper fallback temperatures, for example 0.0,0.2. Used only with STT backend = whisper.",
                );
                numeric_enabled(
                    ui,
                    &language,
                    whisper,
                    "context_min_seconds",
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                    "Minimum utterance length before passing previous context/prompt hints to Whisper. Used only with STT backend = whisper.",
                );
                checkbox_enabled(
                    ui,
                    whisper,
                    "Skip silent hallucinations",
                    &mut self.settings.hallucination_guard,
                    "Local Whisper only: skip long silent gaps where Whisper tends to hallucinate 'like and subscribe'-style text. Adds word timestamps (small extra compute). Used only with STT backend = whisper.",
                );
                numeric_help(
                    ui,
                    &language,
                    "preview_seconds",
                    "Live preview seconds",
                    &mut self.settings.preview_seconds,
                    "While recording, transcribe the buffer this often (seconds) so the live card shows the sentence growing. 0 disables. LOCAL Whisper backend only — ignored for cloud STT. The final result at key release is unchanged.",
                );
                numeric_help(
                    ui,
                    &language,
                    "vad_threshold",
                    "VAD threshold",
                    &mut self.settings.vad_threshold,
                    "Voice activity detection sensitivity (faster-whisper VAD). Lower is more sensitive, higher rejects more noise.",
                );
                numeric_help(
                    ui,
                    &language,
                    "vad_min_silence_ms",
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                    "Silence duration used by the faster-whisper VAD to split or end speech.",
                );
                numeric_help(
                    ui,
                    &language,
                    "vad_speech_pad_ms",
                    "VAD speech pad ms",
                    &mut self.settings.vad_speech_pad_ms,
                    "Audio padding kept around detected speech so soft first and last syllables are not trimmed.",
                );
            },
        );

        // Wave 8 of #348 removed the Parakeet-specific quality group
        // ("Parakeet min seconds") together with the backend.

        ui.add_space(12.0);
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately. It primarily affects Whisper decoding (passed as the faster-whisper `initial_prompt` kwarg) and also feeds dictionary-term matching.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately. It primarily affects Whisper decoding (passed as the faster-whisper `initial_prompt` kwarg) and also feeds dictionary-term matching.");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }
}
