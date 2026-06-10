"""Unit tests for the vp_capture module-level helpers (extracted from Dictate).

test_capture.py drives the CaptureMixin instance methods (arecord reader,
sounddevice start, stop, level metering). These cover the new module-level
plumbing that moved out of ``Dictate.__init__``: the one-shot arecord-device
probe + its caching, and the lazy-global materialisation that lets the capture
methods resolve ``np`` / ``SR`` / ``_find_arecord_device`` from this module.
"""
from helpers import (
    io,
    patch,
    real_numpy,
    redirect_stderr,
    types,
    unittest,
)

from whisper_dictate import vp_capture


def _fake_sd(devices):
    """A minimal sounddevice stub whose ``query_devices()`` returns ``devices``."""
    return types.SimpleNamespace(query_devices=lambda: list(devices))


_DEVICES = [
    {"name": "Built-in Output", "max_input_channels": 0},
    {"name": "MacBook Pro Microphone", "max_input_channels": 1},
    {"name": "Yeti Classic", "max_input_channels": 2},
]


class ResolveSounddeviceDeviceTests(unittest.TestCase):
    def test_empty_value_uses_system_default(self):
        sd = _fake_sd(_DEVICES)
        self.assertIsNone(vp_capture._resolve_sounddevice_device(sd, ""))
        self.assertIsNone(vp_capture._resolve_sounddevice_device(sd, "   "))

    def test_integer_string_is_used_as_device_index(self):
        sd = _fake_sd(_DEVICES)
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "3"), 3)
        # Negative/explicit-sign indices still parse as ints.
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "-1"), -1)

    def test_substring_match_is_case_insensitive_first_wins(self):
        sd = _fake_sd(_DEVICES)
        # "mic" matches the MacBook microphone (index 1) before the Yeti.
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "mic"), 1)
        # Case-insensitive, partial match against the Yeti (index 2).
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "yeti"), 2)

    def test_no_match_warns_and_falls_back_to_default(self):
        sd = _fake_sd(_DEVICES)
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            result = vp_capture._resolve_sounddevice_device(sd, "nonexistent device")
        self.assertIsNone(result)
        line = stderr.getvalue()
        self.assertIn("[cap] audio device", line)
        self.assertIn("nonexistent device", line)
        self.assertIn("using default", line)

    def test_output_only_devices_are_skipped_for_substring_match(self):
        sd = _fake_sd(_DEVICES)
        # "Output" only matches an output device (0 input channels), so no match.
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            result = vp_capture._resolve_sounddevice_device(sd, "Output")
        self.assertIsNone(result)


class InputDevicesTests(unittest.TestCase):
    def test_lists_only_input_devices_with_index(self):
        sd = _fake_sd(_DEVICES)
        devices = vp_capture._input_devices(sd)
        self.assertEqual([d["index"] for d in devices], [1, 2])
        self.assertEqual(devices[0]["name"], "MacBook Pro Microphone")
        self.assertEqual(devices[1]["max_input_channels"], 2)

    def test_query_failure_returns_empty(self):
        def _boom():
            raise RuntimeError("PortAudio not initialized")

        sd = types.SimpleNamespace(query_devices=_boom)
        self.assertEqual(vp_capture._input_devices(sd), [])


class ArecordDeviceArgTests(unittest.TestCase):
    def test_set_value_overrides_probed_default(self):
        self.assertEqual(
            vp_capture._arecord_device_arg("pipewire", "hw:1,0"), "hw:1,0"
        )

    def test_empty_value_keeps_probed_default(self):
        self.assertEqual(vp_capture._arecord_device_arg("pipewire", ""), "pipewire")
        self.assertEqual(vp_capture._arecord_device_arg("pipewire", "   "), "pipewire")


class EnsureArecordDeviceTests(unittest.TestCase):
    def setUp(self):
        # Each test controls the cached device explicitly.
        self._saved = vp_capture._ARECORD_DEVICE
        vp_capture._ARECORD_DEVICE = None

    def tearDown(self):
        vp_capture._ARECORD_DEVICE = self._saved

    def test_ensure_probes_once_and_caches_result(self):
        calls = {"n": 0}

        def fake_probe():
            calls["n"] += 1
            return "pipewire-route"

        with patch.object(vp_capture, "_find_arecord_device", fake_probe):
            first = vp_capture._ensure_arecord_device()
            second = vp_capture._ensure_arecord_device()

        self.assertEqual(first, "pipewire-route")
        self.assertEqual(second, "pipewire-route")
        # Probe runs exactly once; the second call hits the cache.
        self.assertEqual(calls["n"], 1)
        self.assertEqual(vp_capture._arecord_device(), "pipewire-route")

    def test_ensure_caches_none_is_re_probed(self):
        # A None result is falsy but also the "unprobed" sentinel, so the helper
        # re-probes until something truthy (or it stays None ⇒ sounddevice path).
        with patch.object(vp_capture, "_find_arecord_device", lambda: None):
            self.assertIsNone(vp_capture._ensure_arecord_device())
        self.assertIsNone(vp_capture._arecord_device())

    def test_arecord_device_reflects_cached_value(self):
        vp_capture._ARECORD_DEVICE = "default"
        self.assertEqual(vp_capture._arecord_device(), "default")

        def _must_not_probe():
            raise AssertionError("re-probed an already-cached device")

        # _ensure does not re-probe when already cached truthy.
        with patch.object(vp_capture, "_find_arecord_device", _must_not_probe):
            self.assertEqual(vp_capture._ensure_arecord_device(), "default")


class LoadRuntimeModulesTests(unittest.TestCase):
    def test_load_runtime_modules_materialises_capture_globals(self):
        try:
            real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        vp_capture._load_runtime_modules()
        self.assertIsNotNone(vp_capture.np)
        self.assertEqual(vp_capture.SR, 16000)
        self.assertTrue(callable(vp_capture._find_arecord_device))


class CaptureMixinWiringTests(unittest.TestCase):
    def test_dictate_mixes_in_capture_mixin(self):
        from whisper_dictate.vp_dictate import Dictate

        self.assertIn(vp_capture.CaptureMixin, Dictate.__mro__)
        # The capture methods are reachable through the combined class.
        for name in ("_cb", "_arecord_reader", "_emit_audio_level",
                     "_start_arecord", "_start_sounddevice",
                     "_stop_capture_streams", "_recording_seconds"):
            self.assertTrue(hasattr(Dictate, name), f"Dictate missing {name}")

    def test_first_audio_wait_is_reexported_for_backcompat(self):
        from whisper_dictate import runtime, vp_dictate

        self.assertEqual(vp_capture.FIRST_AUDIO_WAIT_S, vp_dictate.FIRST_AUDIO_WAIT_S)
        self.assertEqual(runtime.FIRST_AUDIO_WAIT_S, vp_capture.FIRST_AUDIO_WAIT_S)


if __name__ == "__main__":
    unittest.main()
