"""Tests for the input-device listing used by the UI's microphone picker.

Covers the pure ``select_input_devices`` host-API selector + filter, the
``list_input_devices`` wiring over a stubbed sounddevice, the
``print_audio_devices`` CLI entry (JSON success + sounddevice-unavailable error
path) and the ``--list-audio-devices`` worker flag wiring. sounddevice is
stubbed via sys.modules (the pattern from test_stt.py) so no real audio stack is
touched.
"""
from helpers import (
    _capture_stdout,
    json,
    patch,
    sys,
    types,
    unittest,
)

from whisper_dictate import vp_events


def _fake_sd(devices, default_device=None, hostapis=None):
    namespace = types.SimpleNamespace(
        query_devices=lambda: list(devices),
        default=types.SimpleNamespace(device=default_device),
    )
    if hostapis is not None:
        namespace.query_hostapis = lambda: list(hostapis)
    return namespace


# A Windows-shaped fixture mirroring a real machine: every physical mic appears
# once per host API (MME truncates names to 31 chars, DirectSound / WDM-KS add
# pseudo-device noise). host API order: 0=MME, 1=DirectSound, 2=WASAPI, 3=WDM-KS.
_WINDOWS_HOSTAPIS = [
    {"name": "MME", "default_input_device": 0},
    {"name": "Windows DirectSound", "default_input_device": 16},
    {"name": "Windows WASAPI", "default_input_device": 45},
    {"name": "Windows WDM-KS", "default_input_device": 46},
]

# (index, hostapi, max_input_channels, name) flattened into query_devices dicts.
# The list is sparse-index like the real PortAudio table; we build it so the
# query_devices position == the stated PortAudio index.
def _windows_devices():
    by_index = {
        0: (0, 2, "Microsoft Sound Mapper - Input"),
        1: (0, 2, "Microphone (Yeti Classic)"),
        2: (0, 2, "Microphone (Logitech BRIO)"),
        4: (0, 2, "Microphone (Steam Streaming Mic"),  # MME-truncated
        16: (1, 2, "Primary Sound Capture Driver"),
        17: (1, 2, "Microphone (Yeti Classic)"),
        18: (1, 2, "Microphone (Logitech BRIO)"),
        40: (2, 2, "Microphone (Logitech BRIO)"),
        41: (2, 2, "Microphone (HD Pro Webcam C920)"),
        45: (2, 2, "Microphone (Yeti Classic)"),
        46: (3, 2, "Microphone (Realtek HD Audio Mic input)"),
        56: (3, 2, "Headset (@System32\\drivers\\bthhfenum.sys,#2;...)"),
        71: (3, 0, "Microphone ()"),  # no input channels
    }
    top = max(by_index) + 1
    devices = []
    for index in range(top):
        if index in by_index:
            hostapi, channels, name = by_index[index]
            devices.append(
                {"name": name, "hostapi": hostapi, "max_input_channels": channels}
            )
        else:
            # Filler (e.g. an output device) so list position == PortAudio index.
            devices.append({"name": "Speakers", "hostapi": 0, "max_input_channels": 0})
    return devices


class SelectInputDevicesWindowsTests(unittest.TestCase):
    def test_windows_returns_only_wasapi_mics_each_once_full_names(self):
        devices = vp_events.select_input_devices(
            _windows_devices(),
            _WINDOWS_HOSTAPIS,
            is_windows=True,
            default_index=45,
        )
        names = [d["name"] for d in devices]
        # Exactly the WASAPI host API's input devices, each once, full names, no
        # Sound-Mapper / Primary-Driver / WDM-KS noise, no MME truncation.
        self.assertEqual(
            names,
            [
                "Microphone (Logitech BRIO)",
                "Microphone (HD Pro Webcam C920)",
                "Microphone (Yeti Classic)",
            ],
        )
        # Real PortAudio indices preserved so capture still resolves by index.
        self.assertEqual([d["index"] for d in devices], [40, 41, 45])
        by_index = {d["index"]: d for d in devices}
        self.assertTrue(by_index[45]["default"])
        self.assertFalse(by_index[40]["default"])

    def test_windows_falls_back_to_directsound_when_no_wasapi(self):
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 16},
        ]
        devices = vp_events.select_input_devices(
            _windows_devices(),
            hostapis,
            is_windows=True,
            default_index=17,
        )
        # DirectSound host API == index 1; the Primary-Driver pseudo device is
        # filtered only by channels, so it survives here (it has 2 channels in the
        # fixture) — assert we at least stay on a single host API, full names.
        self.assertTrue(all(d["index"] in (16, 17, 18) for d in devices))
        self.assertIn("Microphone (Yeti Classic)", [d["name"] for d in devices])


class SelectInputDevicesNonWindowsTests(unittest.TestCase):
    def test_linux_single_alsa_host_api_returns_its_devices(self):
        # One ALSA-like host API; the default-host-API path must return its mics.
        hostapis = [{"name": "ALSA", "default_input_device": 1}]
        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Internal Mic", "hostapi": 0, "max_input_channels": 1},
            {"name": "Yeti Classic", "hostapi": 0, "max_input_channels": 2},
        ]
        result = vp_events.select_input_devices(
            devices, hostapis, is_windows=False, default_index=1
        )
        self.assertEqual([d["index"] for d in result], [1, 2])
        self.assertEqual(result[0]["name"], "Internal Mic")
        self.assertTrue(result[0]["default"])

    def test_empty_hostapis_returns_all_inputs(self):
        # No host API table → no filtering by host API (legacy/safe behavior).
        devices = [
            {"name": "Internal Mic", "hostapi": 0, "max_input_channels": 1},
            {"name": "Yeti Classic", "hostapi": 7, "max_input_channels": 2},
        ]
        result = vp_events.select_input_devices(
            devices, [], is_windows=False, default_index=None
        )
        self.assertEqual([d["name"] for d in result], ["Internal Mic", "Yeti Classic"])


_LINUX_DEVICES = [
    {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
    {"name": "Internal Mic", "hostapi": 0, "max_input_channels": 1},
    {"name": "Yeti Classic", "hostapi": 0, "max_input_channels": 2},
]
_LINUX_HOSTAPIS = [{"name": "ALSA", "default_input_device": 1}]


class ListInputDevicesTests(unittest.TestCase):
    def test_returns_only_inputs_with_index_and_channels(self):
        sd = _fake_sd(_LINUX_DEVICES, default_device=(2, 0), hostapis=_LINUX_HOSTAPIS)
        devices = vp_events.list_input_devices(sd)

        self.assertEqual([d["index"] for d in devices], [1, 2])
        self.assertEqual(devices[0]["name"], "Internal Mic")
        self.assertEqual(devices[1]["max_input_channels"], 2)

    def test_marks_the_default_input_device(self):
        sd = _fake_sd(_LINUX_DEVICES, default_device=(2, 0), hostapis=_LINUX_HOSTAPIS)
        by_index = {d["index"]: d for d in vp_events.list_input_devices(sd)}

        self.assertTrue(by_index[2]["default"])
        self.assertFalse(by_index[1]["default"])

    def test_single_int_default_is_honoured(self):
        sd = _fake_sd(_LINUX_DEVICES, default_device=1, hostapis=_LINUX_HOSTAPIS)
        by_index = {d["index"]: d for d in vp_events.list_input_devices(sd)}
        self.assertTrue(by_index[1]["default"])

    def test_no_default_marks_nothing(self):
        sd = _fake_sd(_LINUX_DEVICES, default_device=-1, hostapis=_LINUX_HOSTAPIS)
        self.assertFalse(any(d["default"] for d in vp_events.list_input_devices(sd)))

    def test_blank_named_inputs_are_skipped(self):
        # An empty name would collide with the UI's "" = "(System default)"
        # combo value, so blank/whitespace names must be filtered out.
        sd = _fake_sd(
            [
                {"name": "", "hostapi": 0, "max_input_channels": 2},
                {"name": "   ", "hostapi": 0, "max_input_channels": 1},
                {"name": "Real Mic", "hostapi": 0, "max_input_channels": 1},
            ],
            hostapis=_LINUX_HOSTAPIS,
        )
        devices = vp_events.list_input_devices(sd)
        self.assertEqual([d["name"] for d in devices], ["Real Mic"])

    def test_missing_query_hostapis_degrades_to_all_inputs(self):
        # A stub without query_hostapis (older sounddevice) must not crash; it
        # falls back to listing every input device.
        sd = _fake_sd(_LINUX_DEVICES, default_device=1)
        devices = vp_events.list_input_devices(sd)
        self.assertEqual([d["name"] for d in devices], ["Internal Mic", "Yeti Classic"])


class PrintAudioDevicesTests(unittest.TestCase):
    def test_prints_json_array_and_returns_zero(self):
        fake_sd = _fake_sd(_LINUX_DEVICES, default_device=(2, 0), hostapis=_LINUX_HOSTAPIS)
        with _capture_stdout() as out:
            with patch.dict(sys.modules, {"sounddevice": fake_sd}):
                code = vp_events.print_audio_devices()

        self.assertEqual(code, 0)
        payload = json.loads(out.getvalue())
        self.assertIsInstance(payload, list)
        self.assertEqual([d["name"] for d in payload], ["Internal Mic", "Yeti Classic"])
        self.assertTrue(payload[1]["default"])

    def test_missing_sounddevice_prints_error_object_and_returns_one(self):
        # Force `import sounddevice` to fail by mapping it to None in sys.modules.
        with _capture_stdout() as out:
            with patch.dict(sys.modules, {"sounddevice": None}):
                code = vp_events.print_audio_devices()

        self.assertEqual(code, 1)
        payload = json.loads(out.getvalue())
        self.assertIn("error", payload)

    def test_query_failure_prints_error_object_and_returns_one(self):
        def _boom():
            raise RuntimeError("PortAudio host error")

        fake_sd = types.SimpleNamespace(
            query_devices=_boom, default=types.SimpleNamespace(device=None)
        )
        with _capture_stdout() as out:
            with patch.dict(sys.modules, {"sounddevice": fake_sd}):
                code = vp_events.print_audio_devices()

        self.assertEqual(code, 1)
        payload = json.loads(out.getvalue())
        self.assertIn("error", payload)
        self.assertIn("PortAudio host error", payload["error"])


class ListAudioDevicesFlagTests(unittest.TestCase):
    def test_parser_exposes_list_audio_devices_flag(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args(["--list-audio-devices"])
        self.assertTrue(args.list_audio_devices)

    def test_flag_defaults_off(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args([])
        self.assertFalse(args.list_audio_devices)


if __name__ == "__main__":
    unittest.main()
