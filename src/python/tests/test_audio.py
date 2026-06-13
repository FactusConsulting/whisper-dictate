from helpers import (
    _capture_stdout,
    _env,
    patch,
    RealNumpyAudioCase,
)

class AudioDspTests(RealNumpyAudioCase):
    """Characterisation tests for the audio DSP with REAL numpy. These pin
    current behaviour so the upcoming vp_audio.py extraction is provably
    behaviour-preserving (same asserts, only the import path changes)."""

    # --- _noise_snr ---
    def test_noise_snr_too_few_frames(self):
        a = self.np.zeros(1000, dtype=self.np.float32)
        self.assertEqual(self.vp._noise_snr(a), (-90.0, 0.0))

    def test_noise_snr_constant_signal(self):
        a = self.np.full(480 * 8, 0.5, dtype=self.np.float32)
        noise, snr = self.vp._noise_snr(a)
        self.assertAlmostEqual(noise, -6.0206, places=2)
        self.assertAlmostEqual(snr, 0.0, places=6)

    def test_noise_snr_contrast_has_high_snr(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 1.0 if i % 2 == 0 else 0.001, dtype=np.float32)
            for i in range(10)])
        noise, snr = self.vp._noise_snr(a)
        self.assertGreater(snr, 40.0)
        self.assertLess(noise, -40.0)

    # --- _boost_quiet ---
    def test_boost_quiet_normalises_toward_target(self):
        np = self.np
        a = np.full(1920, 0.01, dtype=np.float32)
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertEqual(out.dtype, np.float32)
        rms = float(np.sqrt(np.mean(out ** 2)))
        self.assertAlmostEqual(20 * np.log10(rms), self.vp.TARGET_DBFS,
                               places=1)

    def test_boost_quiet_never_clips(self):
        np = self.np
        a = np.zeros(1920, dtype=np.float32)
        a[:10] = 0.9
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertLessEqual(float(np.max(np.abs(out))), 0.99 + 1e-6)

    def test_boost_quiet_detail_returns_structured_capture_metrics(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.1 if i % 2 == 0 else 0.002, dtype=np.float32)
            for i in range(10)
        ])

        with _capture_stdout():
            _out, metrics = self.vp._boost_quiet_detail(a)

        self.assertAlmostEqual(metrics.raw_dbfs, -23.0, places=1)
        self.assertAlmostEqual(metrics.peak, 0.1, places=2)
        self.assertGreater(metrics.gain, 1.0)
        self.assertLess(metrics.noise_dbfs, -50.0)
        self.assertGreater(metrics.snr_db, 20.0)
        self.assertEqual(metrics.input_status, "good")

    # --- _trim_trailing_silence (primary anti-hallucination defence) ---
    def test_trim_cuts_trailing_noise_floor_keeping_speech_plus_pad(self):
        # 20 frames of speech then 30 frames at the noise floor: the dead tail is
        # cut, keeping the speech + a 4-frame (~120 ms) decay pad.
        np = self.np
        speech = np.full(480 * 20, 0.2, dtype=np.float32)
        silence = np.full(480 * 30, 0.0005, dtype=np.float32)
        a = np.concatenate([speech, silence])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(len(trimmed), 480 * 24)   # 20 speech + 4 pad frames
        self.assertAlmostEqual(ms, 26 * 30.0, places=3)  # 26 frames removed

    def test_trim_keeps_tight_clip_unchanged(self):
        # Only 3 trailing silence frames (< the pad+minimum): nothing is removed.
        np = self.np
        a = np.concatenate([
            np.full(480 * 20, 0.2, dtype=np.float32),
            np.full(480 * 3, 0.0005, dtype=np.float32),
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(ms, 0.0)
        self.assertEqual(len(trimmed), len(a))

    def test_trim_leaves_all_silence_untouched(self):
        # Uniform clip: loudest == quietest, so there is no silence floor below
        # the speech (gap 0 < the minimum) → left untouched, never trims to empty.
        np = self.np
        a = np.full(480 * 10, 0.0005, dtype=np.float32)
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(ms, 0.0)
        self.assertEqual(len(trimmed), len(a))

    def test_trim_keeps_quietly_trailing_word(self):
        # A quiet trailing word (~24 dB above the noise floor, well above the
        # 12 dB margin) is NOT clipped — only the dead tail at the floor is cut.
        np = self.np
        a = np.concatenate([
            np.full(480 * 10, 0.2, dtype=np.float32),      # loud speech
            np.full(480 * 10, 0.008, dtype=np.float32),    # quiet trailing word
            np.full(480 * 20, 0.0005, dtype=np.float32),   # dead tail
        ])
        trimmed, _ms = self.vp._trim_trailing_silence(a)
        # The quiet word (frames 10-19) survives + 4 pad frames = 24 frames kept.
        self.assertEqual(len(trimmed), 480 * 24)

    def test_trim_too_short_buffer_unchanged(self):
        np = self.np
        a = np.full(480 * 2, 0.2, dtype=np.float32)
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(ms, 0.0)
        self.assertEqual(len(trimmed), len(a))

    def test_trim_preserves_speech_in_final_partial_frame(self):
        # A brief final phoneme landing in the trailing < 30 ms partial frame
        # (after a silence gap) must NOT be trimmed — the remainder is scored too.
        # Without that, the last full speech frame would be index 19, the blip
        # would be ignored, and 26 frames would be wrongly cut.
        np = self.np
        a = np.concatenate([
            np.full(480 * 20, 0.2, dtype=np.float32),     # speech
            np.full(480 * 30, 0.0005, dtype=np.float32),  # gap at the noise floor
            np.full(240, 0.2, dtype=np.float32),          # blip in the partial frame
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(ms, 0.0)
        self.assertEqual(len(trimmed), len(a))

    def test_trim_keeps_soft_trailing_speech_without_silence(self):
        # Continuous speech that trails off SOFTLY with no true silence floor:
        # loudest-vs-quietest gap is only ~20 dB (< the minimum), so there is no
        # clear silence floor → the clip is left untouched rather than risking
        # trimming the soft speech.
        np = self.np
        a = np.concatenate([
            np.full(480 * 20, 0.2, dtype=np.float32),    # loud speech body
            np.full(480 * 20, 0.02, dtype=np.float32),   # soft trailing speech (~20 dB down)
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertEqual(ms, 0.0)
        self.assertEqual(len(trimmed), len(a))

    def test_trim_cuts_long_tail_after_short_speech(self):
        # Short utterance (3 frames) then a long held-key dead tail (47 frames):
        # speech is <10% of frames. The noise floor is the quietest frame
        # regardless of the ratio, and the loud-vs-quiet gap is large, so the dead
        # tail is still cut.
        np = self.np
        a = np.concatenate([
            np.full(480 * 3, 0.2, dtype=np.float32),      # short speech
            np.full(480 * 47, 0.0005, dtype=np.float32),  # long dead tail
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertGreater(ms, 0.0)
        self.assertEqual(len(trimmed), 480 * 7)   # 3 speech + 4 pad frames

    def test_trim_keeps_very_soft_trailing_word_above_noise_floor(self):
        # THE REGRESSION (real repro): a genuinely soft trailing word ~32 dB below
        # the loud body — but still well ABOVE the room-tone noise floor — followed
        # by dead air. The soft word must survive; only the dead air at the noise
        # floor is cut. A "30 dB below the loudest frame" rule would have eaten it.
        np = self.np
        a = np.concatenate([
            np.full(480 * 15, 0.2, dtype=np.float32),     # loud speech body
            np.full(480 * 5, 0.005, dtype=np.float32),    # very soft trailing word
            np.full(480 * 20, 0.0003, dtype=np.float32),  # dead air at the noise floor
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertGreater(ms, 0.0)                       # the dead air IS cut
        self.assertEqual(len(trimmed), 480 * 24)          # soft word (15-19) + 4 pad kept

    def test_trim_robust_to_stray_silent_frame(self):
        # A single fully-silent (RMS 0) dropout frame must NOT collapse the noise
        # floor — that is why the floor is the 10th percentile, not np.min. With
        # min, the zero would set the floor to ~0, drop the threshold to ~0, mark
        # every frame "speech" and trim nothing; the 10th pct keeps room tone as
        # the floor so the real dead tail is still cut.
        np = self.np
        a = np.concatenate([
            np.zeros(480, dtype=np.float32),              # 1 digital-zero dropout frame
            np.full(480 * 19, 0.2, dtype=np.float32),     # speech
            np.full(480 * 30, 0.0005, dtype=np.float32),  # dead tail at room tone
        ])
        trimmed, ms = self.vp._trim_trailing_silence(a)
        self.assertGreater(ms, 0.0)   # dead tail still cut despite the zero frame
        self.assertLess(len(trimmed), len(a))

    def test_cap_line_is_bold_on_interactive_terminal(self):
        from whisper_dictate import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR=None, VOICEPI_NO_COLOR=None):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "\033[1m[cap] raw=-20dBFS\033[0m",
                )

    def test_cap_line_stays_plain_for_piped_output(self):
        from whisper_dictate import vp_audio

        class Pipe:
            def isatty(self):
                return False

        with patch.object(vp_audio.sys, "stdout", Pipe()):
            self.assertEqual(
                vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                "[cap] raw=-20dBFS",
            )
    def test_cap_line_highlight_respects_no_color(self):
        from whisper_dictate import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )
            with _env(VOICEPI_NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )

    def test_input_level_status_labels_actionable_gain_ranges(self):
        from whisper_dictate import vp_audio

        self.assertEqual(vp_audio._input_level_status(-60.0, 0.01, 40.0), "too_quiet")
        self.assertEqual(vp_audio._input_level_status(-35.0, 0.20, 40.0), "good")
        self.assertEqual(vp_audio._input_level_status(-47.0, 0.07, 35.0), "quiet")
        self.assertEqual(vp_audio._input_level_status(-20.0, 0.10, 2.0), "low_snr")
        self.assertEqual(vp_audio._input_level_status(-16.0, 0.30, 35.0), "hot")
        self.assertEqual(vp_audio._input_level_status(-24.0, 0.99, 35.0), "clip_risk")

    def test_cap_line_reports_input_level_status(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.1 if i % 2 == 0 else 0.002, dtype=np.float32)
            for i in range(10)
        ])

        with _capture_stdout() as buf:
            self.vp._boost_quiet(a)

        self.assertIn("input=good", buf.getvalue())

    # --- _looks_like_speech ---
    def test_looks_like_speech_rejects_too_quiet(self):
        a = self.np.full(1920, 1e-4, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("too quiet", msg)
        self.assertIn("input=too_quiet", msg)

    def test_looks_like_speech_rejects_flat_signal(self):
        a = self.np.full(1920, 0.1, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("no speech contrast", msg)
        self.assertIn("input=low_snr", msg)

    def test_looks_like_speech_accepts_contrasted_speech(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(10)])
        ok, _ = self.vp._looks_like_speech(a)
        self.assertTrue(ok)

    def test_audio_level_metrics_use_rms_not_peak_for_live_meter(self):
        np = self.np
        pcm = np.zeros((16000, 1), dtype=np.int16)
        pcm[0, 0] = 32767

        raw_dbfs, peak, level = self.vp._audio_level_metrics(pcm)

        self.assertAlmostEqual(peak, 1.0, places=3)
        self.assertLess(raw_dbfs, -40.0)
        self.assertLess(level, 0.3)

    def test_audio_level_metrics_map_normal_speech_to_visible_meter(self):
        np = self.np
        pcm = (np.full((16000, 1), 0.1, dtype=np.float32) * 32767).astype(np.int16)

        raw_dbfs, peak, level = self.vp._audio_level_metrics(pcm)

        self.assertAlmostEqual(raw_dbfs, -20.0, places=1)
        self.assertAlmostEqual(peak, 0.1, places=2)
        self.assertGreater(level, 0.7)

    def test_select_active_channel_pcm_preserves_loudest_stereo_channel(self):
        np = self.np
        left = np.zeros(16000, dtype=np.int16)
        right = (np.full(16000, 0.1, dtype=np.float32) * 32767).astype(np.int16)
        stereo = np.stack([left, right], axis=1)

        mono = self.vp._select_active_channel_pcm(stereo)

        self.assertEqual(mono.shape, (16000, 1))
        self.assertAlmostEqual(float(np.max(np.abs(mono))) / 32768.0, 0.1, places=2)

    def test_select_active_channel_pcm_supports_multichannel_interfaces(self):
        np = self.np
        channels = [
            np.zeros(16000, dtype=np.int16),
            (np.full(16000, 0.02, dtype=np.float32) * 32767).astype(np.int16),
            np.zeros(16000, dtype=np.int16),
            (np.full(16000, 0.12, dtype=np.float32) * 32767).astype(np.int16),
        ]
        multichannel = np.stack(channels, axis=1)

        mono = self.vp._select_active_channel_pcm(multichannel)

        self.assertEqual(mono.shape, (16000, 1))
        self.assertAlmostEqual(float(np.max(np.abs(mono))) / 32768.0, 0.12, places=2)

    def test_audio_level_metrics_use_active_stereo_channel_for_live_meter(self):
        np = self.np
        left = np.zeros(16000, dtype=np.int16)
        right = (np.full(16000, 0.1, dtype=np.float32) * 32767).astype(np.int16)
        stereo = np.stack([left, right], axis=1)

        raw_dbfs, peak, level = self.vp._audio_level_metrics(stereo)

        self.assertAlmostEqual(raw_dbfs, -20.0, places=1)
        self.assertAlmostEqual(peak, 0.1, places=2)
        self.assertGreater(level, 0.7)


