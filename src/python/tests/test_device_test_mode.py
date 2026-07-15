"""Tests for the ``--test-audio-device`` dry-run microphone usability check.

The dry run reuses the SAME open matrix as live capture (the WASAPI →
DirectSound → MME sweep), opening each candidate and immediately closing it
without capturing audio. These tests drive
``vp_device_test.dry_run_test_device`` / ``test_audio_device`` with a FAKE
sounddevice (no real audio stack touched), asserting:

  * a device that opens reports ``usable: true`` with the right endpoint/rate/dtype,
  * a device that fails on every endpoint reports ``usable: false`` + reason,
  * an unresolved name reports ``usable: false`` / reason "device not found",
  * NO long-lived stream survives the call (every opened stream is closed),
  * the entry point never raises (missing sounddevice / a query that explodes
    are reported as ``usable: false``).
"""
from helpers import (
    _capture_stdout,
    json,
    patch,
    sys,
    types,
    unittest,
)

from whisper_dictate import vp_device_test


class _Stream:
    """A fake InputStream that records open kwargs and tracks stop/close.

    The class-level ``opened`` / ``live`` lists let a test assert which
    candidates were tried and that NOTHING stayed open after the dry run. An
    ``accept`` predicate decides which kwargs succeed (others raise, exactly like
    a PortAudio format rejection).
    """

    opened: list = []
    live: list = []
    accept = staticmethod(lambda kwargs: True)

    def __init__(self, **kwargs):
        type(self).opened.append(kwargs)
        if not type(self).accept(kwargs):
            raise RuntimeError("format unsupported")
        self.kwargs = kwargs
        self.started = False
        type(self).live.append(self)

    def start(self):
        self.started = True

    def stop(self):
        self.started = False

    def close(self):
        if self in type(self).live:
            type(self).live.remove(self)


def _make_stream_class(accept):
    return type("Stream", (_Stream,), {
        "opened": [],
        "live": [],
        "accept": staticmethod(accept),
    })


_LINUX_DEVICES = [
    {"name": "Speakers", "hostapi": 0, "max_input_channels": 0,
     "default_samplerate": 48000.0},
    {"name": "Internal Mic", "hostapi": 0, "max_input_channels": 2,
     "default_samplerate": 48000.0},
    {"name": "Yeti Classic", "hostapi": 0, "max_input_channels": 2,
     "default_samplerate": 48000.0},
]
_LINUX_HOSTAPIS = [{"name": "ALSA", "default_input_device": 1}]


def _fake_sd(devices, hostapis, stream_cls, *, default_device=None,
             wasapi_settings=False):
    def query_devices(device=None, kind=None):
        if device is None and kind is None:
            return list(devices)
        if kind == "input":
            idx = default_device if isinstance(default_device, int) else 1
            return devices[idx]
        return devices[device]

    ns = types.SimpleNamespace(
        InputStream=stream_cls,
        query_devices=query_devices,
        query_hostapis=lambda: list(hostapis),
        default=types.SimpleNamespace(device=default_device),
    )
    if wasapi_settings:
        ns.WasapiSettings = lambda auto_convert=False: types.SimpleNamespace(
            auto_convert=auto_convert)
    return ns


class DryRunUsableTests(unittest.TestCase):
    def test_named_device_opens_reports_usable_with_endpoint_and_rate(self):
        # Every 16k int16 open succeeds → first candidate wins, no resample.
        stream_cls = _make_stream_class(lambda kwargs: True)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        result = vp_device_test.dry_run_test_device(sd, "Yeti")

        self.assertTrue(result["usable"])
        self.assertEqual(result["device"], "Yeti Classic")
        self.assertEqual(result["endpoint"], "default")  # ALSA single host API
        self.assertEqual(result["samplerate"], 16000)
        self.assertEqual(result["dtype"], "int16")
        self.assertFalse(result["resampled"])
        self.assertIsNone(result["reason"])
        # CRITICAL: no stream stayed open after the dry run.
        self.assertEqual(stream_cls.live, [])

    def test_system_default_when_value_empty(self):
        stream_cls = _make_stream_class(lambda kwargs: True)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        result = vp_device_test.dry_run_test_device(sd, "")

        self.assertTrue(result["usable"])
        # Empty value resolves to the preferred host API's default input.
        self.assertEqual(result["dtype"], "int16")
        self.assertEqual(stream_cls.live, [])

    def test_device_needing_native_rate_reports_resampled(self):
        # Reject every 16k open; accept only the device-native 48k open. The
        # native-rate fallback must win and report resampled=true.
        def accept(kwargs):
            return kwargs.get("samplerate") == 48000

        stream_cls = _make_stream_class(accept)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        result = vp_device_test.dry_run_test_device(sd, "Yeti")

        self.assertTrue(result["usable"])
        self.assertEqual(result["samplerate"], 48000)
        self.assertTrue(result["resampled"])
        self.assertEqual(stream_cls.live, [])


class DryRunUnusableTests(unittest.TestCase):
    def test_all_endpoints_fail_reports_unusable_with_reason(self):
        stream_cls = _make_stream_class(lambda kwargs: False)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        result = vp_device_test.dry_run_test_device(sd, "Yeti")

        self.assertFalse(result["usable"])
        self.assertEqual(result["device"], "Yeti Classic")
        self.assertIsNone(result["endpoint"])
        self.assertIsNone(result["samplerate"])
        self.assertIn("could not open", result["reason"])
        self.assertEqual(stream_cls.live, [])

    def test_unresolved_name_reports_device_not_found(self):
        # No open is ever attempted for a name that resolves to nothing.
        stream_cls = _make_stream_class(lambda kwargs: True)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        result = vp_device_test.dry_run_test_device(sd, "Nonexistent Mic XYZ")

        self.assertFalse(result["usable"])
        self.assertEqual(result["reason"], "device not found")
        self.assertEqual(stream_cls.opened, [])  # never tried to open
        self.assertEqual(stream_cls.live, [])


class WindowsSiblingFallbackTests(unittest.TestCase):
    """A WASAPI endpoint that won't open but whose DirectSound sibling does."""

    _HOSTAPIS = [
        {"name": "MME", "default_input_device": 0},
        {"name": "Windows DirectSound", "default_input_device": 1},
        {"name": "Windows WASAPI", "default_input_device": 2},
    ]
    # index 0 = MME sibling, 1 = DirectSound sibling, 2 = resolved WASAPI endpoint.
    _DEVICES = [
        {"name": "Microphone (Yeti Classic)", "hostapi": 0,
         "max_input_channels": 2, "default_samplerate": 48000.0},
        {"name": "Microphone (Yeti Classic)", "hostapi": 1,
         "max_input_channels": 2, "default_samplerate": 48000.0},
        {"name": "Microphone (Yeti Classic)", "hostapi": 2,
         "max_input_channels": 2, "default_samplerate": 48000.0},
    ]

    def test_wasapi_fails_directsound_sibling_opens(self):
        # WASAPI endpoint (device index 2) rejects everything; the DirectSound
        # sibling (index 1) accepts 16k int16. The result must report the
        # DirectSound endpoint and usable=true (same physical mic).
        def accept(kwargs):
            return kwargs.get("device") == 1

        stream_cls = _make_stream_class(accept)
        sd = _fake_sd(self._DEVICES, self._HOSTAPIS, stream_cls,
                      default_device=2, wasapi_settings=True)

        with patch("os.name", "nt"):
            result = vp_device_test.dry_run_test_device(sd, "Yeti")

        self.assertTrue(result["usable"])
        self.assertEqual(result["endpoint"], "directsound")
        self.assertFalse(result["resampled"])  # 16k int16 direct on DirectSound
        self.assertEqual(stream_cls.live, [])


class TestAudioDeviceEntryTests(unittest.TestCase):
    def test_prints_single_json_object_and_exits_zero(self):
        stream_cls = _make_stream_class(lambda kwargs: True)
        sd = _fake_sd(_LINUX_DEVICES, _LINUX_HOSTAPIS, stream_cls,
                      default_device=1)

        with _capture_stdout() as out:
            code = vp_device_test.test_audio_device("Yeti", sd_factory=lambda: sd)

        self.assertEqual(code, 0)
        # Exactly one JSON object on stdout.
        payload = json.loads(out.getvalue())
        self.assertTrue(payload["usable"])
        self.assertEqual(payload["device"], "Yeti Classic")

    def test_missing_sounddevice_reports_unusable_not_raises(self):
        def _boom():
            raise ImportError("No module named 'sounddevice'")

        with _capture_stdout() as out:
            code = vp_device_test.test_audio_device("Yeti", sd_factory=_boom)

        self.assertEqual(code, 0)
        payload = json.loads(out.getvalue())
        self.assertFalse(payload["usable"])
        self.assertIn("sounddevice unavailable", payload["reason"])

    def test_unexpected_error_during_probe_reports_unusable(self):
        # query_devices explodes mid-resolution → caught, reported, never raised.
        def _boom_devices(device=None, kind=None):
            raise RuntimeError("PortAudio host error")

        sd = types.SimpleNamespace(
            query_devices=_boom_devices,
            query_hostapis=lambda: [],
            default=types.SimpleNamespace(device=None),
        )
        with _capture_stdout() as out:
            code = vp_device_test.test_audio_device("Yeti", sd_factory=lambda: sd)

        self.assertEqual(code, 0)
        payload = json.loads(out.getvalue())
        # An empty/failed resolution of an unresolved name → device not found.
        self.assertFalse(payload["usable"])


class TestAudioDeviceFlagTests(unittest.TestCase):
    def test_parser_exposes_test_audio_device_flag(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args(["--test-audio-device", "Yeti"])
        self.assertEqual(args.test_audio_device, "Yeti")

    def test_flag_defaults_none(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args([])
        self.assertIsNone(args.test_audio_device)

    def test_flag_accepts_empty_string_for_system_default(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args(["--test-audio-device", ""])
        self.assertEqual(args.test_audio_device, "")


if __name__ == "__main__":
    unittest.main()
