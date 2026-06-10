"""Tests for the input-device listing used by the UI's microphone picker.

Covers the pure ``list_input_devices`` helper, the ``print_audio_devices`` CLI
entry (JSON success + sounddevice-unavailable error path) and the
``--list-audio-devices`` worker flag wiring. sounddevice is stubbed via
sys.modules (the pattern from test_stt.py) so no real audio stack is touched.
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


def _fake_sd(devices, default_device=None):
    return types.SimpleNamespace(
        query_devices=lambda: list(devices),
        default=types.SimpleNamespace(device=default_device),
    )


_DEVICES = [
    {"name": "Speakers", "max_input_channels": 0},
    {"name": "Internal Mic", "max_input_channels": 1},
    {"name": "Yeti Classic", "max_input_channels": 2},
]


class ListInputDevicesTests(unittest.TestCase):
    def test_returns_only_inputs_with_index_and_channels(self):
        sd = _fake_sd(_DEVICES, default_device=(2, 0))
        devices = vp_events.list_input_devices(sd)

        self.assertEqual([d["index"] for d in devices], [1, 2])
        self.assertEqual(devices[0]["name"], "Internal Mic")
        self.assertEqual(devices[1]["max_input_channels"], 2)

    def test_marks_the_default_input_device(self):
        sd = _fake_sd(_DEVICES, default_device=(2, 0))
        devices = vp_events.list_input_devices(sd)
        by_index = {d["index"]: d for d in devices}

        self.assertTrue(by_index[2]["default"])
        self.assertFalse(by_index[1]["default"])

    def test_single_int_default_is_honoured(self):
        sd = _fake_sd(_DEVICES, default_device=1)
        by_index = {d["index"]: d for d in vp_events.list_input_devices(sd)}
        self.assertTrue(by_index[1]["default"])

    def test_no_default_marks_nothing(self):
        sd = _fake_sd(_DEVICES, default_device=-1)
        self.assertFalse(any(d["default"] for d in vp_events.list_input_devices(sd)))

    def test_blank_named_inputs_are_skipped(self):
        # An empty name would collide with the UI's "" = "(System default)"
        # combo value, so blank/whitespace names must be filtered out.
        sd = _fake_sd([
            {"name": "", "max_input_channels": 2},
            {"name": "   ", "max_input_channels": 1},
            {"name": "Real Mic", "max_input_channels": 1},
        ])
        devices = vp_events.list_input_devices(sd)
        self.assertEqual([d["name"] for d in devices], ["Real Mic"])


class PrintAudioDevicesTests(unittest.TestCase):
    def test_prints_json_array_and_returns_zero(self):
        fake_sd = _fake_sd(_DEVICES, default_device=(2, 0))
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
