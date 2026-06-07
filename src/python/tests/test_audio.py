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

        self.assertEqual(round(peak, 3), 1.0)
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


