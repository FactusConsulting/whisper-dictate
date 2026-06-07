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

    def _target(self, backend="whisper", parakeet_min=1.5):
        return types.SimpleNamespace(
            stt_backend=backend, parakeet_min_seconds=parakeet_min)

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
