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


def _fake_sd(devices, *, hostapis=None, default_device=None):
    """A sounddevice stub whose ``query_devices()`` returns ``devices``.

    Optionally exposes ``query_hostapis()`` and ``default.device`` so the
    host-API-aware capture resolver (``resolve_capture_device``) can be driven
    with the same multi-host-API table the picker uses.
    """
    ns = types.SimpleNamespace(
        query_devices=lambda: list(devices),
        default=types.SimpleNamespace(device=default_device),
    )
    if hostapis is not None:
        ns.query_hostapis = lambda: list(hostapis)
    return ns


_DEVICES = [
    {"name": "Built-in Output", "max_input_channels": 0},
    {"name": "MacBook Pro Microphone", "max_input_channels": 1},
    {"name": "Yeti Classic", "max_input_channels": 2},
]


class ResolveSounddeviceDeviceTests(unittest.TestCase):
    """The ``(index, name)`` contract on a single-host-API (non-Windows) table.

    Windows multi-host-API resolution (WASAPI preference) is covered by
    ResolveCaptureDeviceWindowsTests below.
    """

    def test_empty_value_uses_system_default(self):
        # No hostapis + no default → nothing to bind, sounddevice picks default.
        sd = _fake_sd(_DEVICES)
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, ""), (None, None))
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "   "), (None, None))

    def test_integer_string_is_used_as_device_index(self):
        sd = _fake_sd(_DEVICES)
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "3"), (3, None))
        # Negative/explicit-sign indices still parse as ints.
        self.assertEqual(vp_capture._resolve_sounddevice_device(sd, "-1"), (-1, None))

    def test_substring_match_is_case_insensitive(self):
        sd = _fake_sd(_DEVICES)
        # "mic" matches the MacBook microphone (index 1, full name carried back).
        self.assertEqual(
            vp_capture._resolve_sounddevice_device(sd, "mic"),
            (1, "MacBook Pro Microphone"),
        )
        # Case-insensitive, partial match against the Yeti (index 2).
        self.assertEqual(
            vp_capture._resolve_sounddevice_device(sd, "yeti"),
            (2, "Yeti Classic"),
        )

    def test_no_match_warns_and_falls_back_to_default(self):
        sd = _fake_sd(_DEVICES)
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            result = vp_capture._resolve_sounddevice_device(sd, "nonexistent device")
        self.assertEqual(result, (None, None))
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
        self.assertEqual(result, (None, None))


# A Windows multi-host-API table mirroring the verified dev box: the SAME
# physical Jabra mic appears under MME (name truncated to 31 chars) and under
# WASAPI / DirectSound (full 39-char name). host API order: 0=MME,
# 1=DirectSound, 2=WASAPI.
_JABRA_FULL = "Headset Microphone (Jabra Evolve 65 TE)"   # 39 chars, WASAPI/DS
_JABRA_MME = "Headset Microphone (Jabra Evolv"            # 31 chars, MME
_WIN_HOSTAPIS = [
    {"name": "MME", "default_input_device": 0},
    {"name": "Windows DirectSound", "default_input_device": 3},
    {"name": "Windows WASAPI", "default_input_device": 5},
]
_WIN_DEVICES = [
    {"name": "Microsoft Sound Mapper - Input", "hostapi": 0, "max_input_channels": 2},
    {"name": _JABRA_MME, "hostapi": 0, "max_input_channels": 2},           # 1 MME
    {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
    {"name": _JABRA_FULL, "hostapi": 1, "max_input_channels": 2},          # 3 DSound
    {"name": "Primary Sound Capture Driver", "hostapi": 1, "max_input_channels": 2},
    {"name": _JABRA_FULL, "hostapi": 2, "max_input_channels": 2},          # 5 WASAPI
]


class ResolveCaptureDeviceWindowsTests(unittest.TestCase):
    """Windows host-API-aware capture resolution (the heart of the fix).

    Drives the pure ``vp_events.resolve_capture_device`` with the verified
    multi-host-API table so the WASAPI-preference + full-name + MME-truncation
    tolerance are unit-tested without opening a stream.
    """

    def _resolve(self, value, *, is_windows=True, default_index=None):
        from whisper_dictate import vp_events
        return vp_events.resolve_capture_device(
            _WIN_DEVICES, _WIN_HOSTAPIS, value,
            is_windows=is_windows, default_index=default_index,
        )

    def test_saved_full_name_resolves_to_wasapi_index_full_name(self):
        index, name = self._resolve(_JABRA_FULL)
        # WASAPI host API (2) is preferred over MME (0) / DirectSound (1).
        self.assertEqual(index, 5)
        self.assertEqual(name, _JABRA_FULL)

    def test_saved_mme_truncated_name_resolves_to_full_wasapi_device(self):
        # An OLD saved value is the 31-char MME truncation; it must still bind
        # the full-name WASAPI device (bidirectional-substring match).
        index, name = self._resolve(_JABRA_MME)
        self.assertEqual(index, 5)
        self.assertEqual(name, _JABRA_FULL)

    def test_default_fallback_picks_wasapi_default_full_name(self):
        # Empty value → the WASAPI host API's default_input_device (5), full name,
        # NOT the global MME default (0, truncated).
        index, name = self._resolve("", default_index=0)
        self.assertEqual(index, 5)
        self.assertEqual(name, _JABRA_FULL)

    def test_exact_name_wins_over_longer_sibling_prefix(self):
        # Regression: a saved value that is a clean PREFIX of a longer sibling
        # in the SAME host API ("Microphone" vs "Microphone Array") must bind
        # the EXACT device, not the longer sibling that longest-substring would
        # otherwise win.
        from whisper_dictate import vp_events
        devices = [
            {"name": "Microphone", "hostapi": 2, "max_input_channels": 2},        # 0
            {"name": "Microphone Array", "hostapi": 2, "max_input_channels": 2},  # 1
        ]
        index, name = vp_events.resolve_capture_device(
            devices, _WIN_HOSTAPIS, "Microphone", is_windows=True, default_index=None,
        )
        self.assertEqual(index, 0)
        self.assertEqual(name, "Microphone")

    def test_exact_match_is_case_insensitive_over_sibling_prefix(self):
        # The exact-match precedence is case-insensitive (casefold), and still
        # beats a longer sibling regardless of candidate order.
        from whisper_dictate import vp_events
        devices = [
            {"name": "Microphone Array", "hostapi": 2, "max_input_channels": 2},  # 0
            {"name": "Microphone", "hostapi": 2, "max_input_channels": 2},        # 1
        ]
        index, name = vp_events.resolve_capture_device(
            devices, _WIN_HOSTAPIS, "microphone", is_windows=True, default_index=None,
        )
        self.assertEqual(index, 1)
        self.assertEqual(name, "Microphone")

    def test_truncated_prefix_still_resolves_when_no_exact_match(self):
        # The MME-truncation tolerance is preserved: with NO exact match, the
        # 31-char truncated saved value still resolves (longest-substring) to
        # the single full-name WASAPI device.
        index, name = self._resolve(_JABRA_MME)
        self.assertEqual(index, 5)
        self.assertEqual(name, _JABRA_FULL)

    def test_integer_value_is_used_verbatim(self):
        self.assertEqual(self._resolve("5"), (5, None))

    def test_non_windows_single_host_api_unchanged(self):
        # A single ALSA host API: resolution must behave like the legacy
        # first-substring-match, never preferring a Windows API.
        from whisper_dictate import vp_events
        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Internal Mic", "hostapi": 0, "max_input_channels": 1},
            {"name": "Yeti Classic", "hostapi": 0, "max_input_channels": 2},
        ]
        hostapis = [{"name": "ALSA", "default_input_device": 1}]
        index, name = vp_events.resolve_capture_device(
            devices, hostapis, "yeti", is_windows=False, default_index=1,
        )
        self.assertEqual((index, name), (2, "Yeti Classic"))
        # Default fallback uses the ALSA host API's default input (index 1).
        self.assertEqual(
            vp_events.resolve_capture_device(
                devices, hostapis, "", is_windows=False, default_index=1),
            (1, "Internal Mic"),
        )


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
