"""Unit tests for vp_dictate internals (extracted from runtime.py).

The end-to-end loop is exercised by test_dictate_loop.py / test_capture.py and
the source-layout guards by test_audio.py. These add focused coverage for the
small pure helpers that live with the Dictate class: XKB layout normalisation,
the too-short/Parakeet capture gate, and the lazy ``_load_runtime_modules``
materialising this module's transcribe-side globals.
"""
from helpers import (
    _capture_stdout,
    _env,
    real_numpy,
    types,
    unittest,
)

from whisper_dictate import vp_capture, vp_dictate, vp_keymap


class NormalizeXkbTests(unittest.TestCase):
    def test_blank_returns_none(self):
        self.assertIsNone(vp_keymap._normalize_xkb_layout(None))
        self.assertIsNone(vp_keymap._normalize_xkb_layout("  "))

    def test_lang_codes_map_to_xkb(self):
        self.assertEqual(vp_keymap._normalize_xkb_layout("da"), "dk")
        self.assertEqual(vp_keymap._normalize_xkb_layout("nb"), "no")
        self.assertEqual(vp_keymap._normalize_xkb_layout("sv"), "se")

    def test_direct_supported_layout_passes(self):
        self.assertEqual(vp_keymap._normalize_xkb_layout("de"), "de")
        self.assertEqual(vp_keymap._normalize_xkb_layout("us"), "us")

    def test_unsupported_layout_returns_none(self):
        self.assertIsNone(vp_keymap._normalize_xkb_layout("xx"))


class DetectXkbTests(unittest.TestCase):
    def test_env_override_wins(self):
        with _env(VOICEPI_XKB_LAYOUT="dk", XKB_DEFAULT_LAYOUT=None):
            self.assertEqual(vp_keymap._detect_xkb_layout("de"), "dk")

    def test_falls_back_to_lang_when_no_env_or_file(self):
        with _env(VOICEPI_XKB_LAYOUT=None, XKB_DEFAULT_LAYOUT=None):
            # /etc/default/keyboard is absent on CI/Windows -> uses lang hint.
            self.assertEqual(vp_keymap._detect_xkb_layout("da"), "dk")


class ShouldSkipPcmTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.np = real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        vp_dictate._load_runtime_modules()

    def _target(self, backend="whisper", parakeet_min=1.5, min_record=0.5):
        return types.SimpleNamespace(
            stt_backend=backend, parakeet_min_seconds=parakeet_min,
            min_record_seconds=min_record)

    def _pcm(self, samples):
        return self.np.zeros((samples, 1), dtype=self.np.int16)

    def test_too_short_capture_is_skipped(self):
        t = self._target()
        with _capture_stdout() as out:
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(1000), 0.06)
        self.assertTrue(skip)
        self.assertIn("too short", out.getvalue())

    def test_long_enough_whisper_capture_is_kept(self):
        t = self._target()
        with _capture_stdout():
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(16000), 1.0)
        self.assertFalse(skip)

    def test_parakeet_below_min_duration_is_skipped(self):
        t = self._target(backend="parakeet", parakeet_min=1.5)
        with _capture_stdout() as out:
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(16000), 1.0)
        self.assertTrue(skip)
        self.assertIn("too short for Parakeet", out.getvalue())

    def test_parakeet_above_min_duration_is_kept(self):
        t = self._target(backend="parakeet", parakeet_min=1.5)
        with _capture_stdout():
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(32000), 2.0)
        self.assertFalse(skip)

    def test_min_record_seconds_drops_clip_below_setting(self):
        # 0.45 s clip (7200 samples @ 16 kHz) dropped at the default 0.5 floor.
        t = self._target(min_record=0.5)
        with _capture_stdout() as out:
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(7200), 0.45)
        self.assertTrue(skip)
        self.assertIn("too short", out.getvalue())

    def test_min_record_seconds_passes_clip_at_lower_setting(self):
        # Same 0.45 s clip passes when min_record_seconds is lowered to 0.3.
        t = self._target(min_record=0.3)
        with _capture_stdout():
            skip = vp_dictate.Dictate._should_skip_pcm(t, self._pcm(7200), 0.45)
        self.assertFalse(skip)

    def test_min_record_seconds_floor_clamps_below_point_three(self):
        # Setting 0 still enforces the 0.3 s misfire floor: a 0.25 s clip
        # (4000 samples) is dropped, a 0.35 s clip (5600 samples) survives.
        t = self._target(min_record=0.0)
        with _capture_stdout():
            self.assertTrue(
                vp_dictate.Dictate._should_skip_pcm(t, self._pcm(4000), 0.25))
            self.assertFalse(
                vp_dictate.Dictate._should_skip_pcm(t, self._pcm(5600), 0.35))


class LiveReloadGuardSettingsTests(unittest.TestCase):
    """Both anti-hallucination guards live-reload via _apply_runtime_module_config."""

    @classmethod
    def setUpClass(cls):
        try:
            real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        vp_dictate._load_runtime_modules()

    def test_apply_runtime_module_config_reloads_both_guards(self):
        from whisper_dictate import vp_transcribe

        target = types.SimpleNamespace()
        # Mutated config overlay (the dict apply_config_to_environ produces).
        vp_dictate.Dictate._apply_runtime_module_config(
            target,
            {"min_record_seconds": "0.9", "max_chars_per_second": "12"},
        )
        # min_record_seconds is cached on the instance; the rate cap lands on the
        # vp_transcribe module global the gate reads.
        self.assertEqual(target.min_record_seconds, 0.9)
        self.assertEqual(vp_transcribe.MAX_CHARS_PER_SECOND, 12.0)

        # A new overlay takes effect without re-import.
        vp_dictate.Dictate._apply_runtime_module_config(
            target,
            {"min_record_seconds": "0.4", "max_chars_per_second": "0"},
        )
        self.assertEqual(target.min_record_seconds, 0.4)
        self.assertEqual(vp_transcribe.MAX_CHARS_PER_SECOND, 0.0)
        # Reset to default so other tests see the shipped cap.
        vp_transcribe.MAX_CHARS_PER_SECOND = 30.0


class LoadRuntimeModulesTests(unittest.TestCase):
    def test_load_runtime_modules_materialises_transcribe_globals(self):
        try:
            real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        vp_dictate._load_runtime_modules()
        self.assertIsNotNone(vp_dictate.np)
        self.assertEqual(vp_dictate.SR, 16000)
        self.assertTrue(callable(vp_dictate._transcribe_detail))
        self.assertTrue(callable(vp_dictate.is_hallucination))
        # _load_runtime_modules also materialises the capture-side globals,
        # whose home is now vp_capture (arecord probe + numpy + SR).
        self.assertIsNotNone(vp_capture.np)
        self.assertEqual(vp_capture.SR, 16000)
        self.assertTrue(callable(vp_capture._find_arecord_device))


if __name__ == "__main__":
    unittest.main()
