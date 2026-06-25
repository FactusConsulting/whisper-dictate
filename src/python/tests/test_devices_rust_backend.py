"""Tests for the ``VOICEPI_DEVICES_BACKEND=rust`` shell-out in vp_devices.

Phase 2.2.z of the Python-removal roadmap (#348). The Rust port lives in
``src/rust/devices.rs`` and is reached via ``whisper-dictate devices``; this
file exercises the Python fallback: when the env var is unset OR the helper
fails, the existing sounddevice path must keep working unchanged.
"""
from __future__ import annotations

import json
import os
import sys
import types
import unittest
from unittest import mock


# Ensure the package is importable (mirrors conftest's path injection — kept
# defensive in case the test is run in isolation).
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from whisper_dictate import vp_devices  # noqa: E402


def _fake_sd(devices, default_device=None, hostapis=None):
    """Stub matching the shape select_input_devices wants — same as the
    existing test_audio_device_listing fixtures, copied here so this file
    does not have to import the helpers module."""
    namespace = types.SimpleNamespace(
        query_devices=lambda: list(devices),
        default=types.SimpleNamespace(device=default_device),
    )
    if hostapis is not None:
        namespace.query_hostapis = lambda: list(hostapis)
    return namespace


class RustBackendShellOutTests(unittest.TestCase):
    """Drive vp_devices.list_input_devices through the Rust shell-out path."""

    def setUp(self) -> None:
        # Snapshot + restore the env vars we touch so individual tests can be
        # run in any order without leaking state.
        self._prev_backend = os.environ.get("VOICEPI_DEVICES_BACKEND")
        self._prev_helper = os.environ.get("VOICEPI_RUST_INJECTOR")

    def tearDown(self) -> None:
        for key, prev in (
            ("VOICEPI_DEVICES_BACKEND", self._prev_backend),
            ("VOICEPI_RUST_INJECTOR", self._prev_helper),
        ):
            if prev is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = prev

    # --- env-var gating ------------------------------------------------------

    def test_unset_env_falls_through_to_python(self):
        os.environ.pop("VOICEPI_DEVICES_BACKEND", None)
        os.environ["VOICEPI_RUST_INJECTOR"] = "/nonexistent/helper"
        sd = _fake_sd(
            devices=[{"name": "Mic", "max_input_channels": 1, "hostapi": 0}],
            default_device=0,
            hostapis=[{"name": "ALSA", "default_input_device": 0}],
        )
        with mock.patch("whisper_dictate.vp_devices.subprocess.run") as run:
            result = vp_devices.list_input_devices(sd)
        run.assert_not_called()
        self.assertEqual(result[0]["name"], "Mic")

    def test_env_set_without_helper_falls_through(self):
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)
        sd = _fake_sd(
            devices=[{"name": "Mic", "max_input_channels": 1, "hostapi": 0}],
            default_device=0,
            hostapis=[{"name": "ALSA", "default_input_device": 0}],
        )
        with mock.patch("whisper_dictate.vp_devices.subprocess.run") as run:
            result = vp_devices.list_input_devices(sd)
        run.assert_not_called()
        self.assertEqual(result[0]["name"], "Mic")

    # --- happy path ----------------------------------------------------------

    def test_rust_helper_success_replaces_python_enumeration(self):
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        # If the Rust path is taken, sd must NEVER be queried — the stub
        # blows up if it is.
        sd = types.SimpleNamespace(
            query_devices=mock.Mock(side_effect=AssertionError("sd should not be touched")),
            query_hostapis=mock.Mock(side_effect=AssertionError("sd should not be touched")),
            default=types.SimpleNamespace(device=None),
        )
        rust_payload = {
            "devices": [
                {
                    "index": 0,
                    "name": "Headset Microphone (Jabra Evolve 65 TE)",
                    "max_input_channels": 1,
                    "sample_rates": [16000, 48000],
                    "default": True,
                },
                {
                    "index": 1,
                    "name": "Webcam Mic",
                    "max_input_channels": 2,
                    "sample_rates": [44100, 44100],
                    "default": False,
                },
            ]
        }
        completed = types.SimpleNamespace(
            returncode=0,
            stdout=json.dumps(rust_payload),
            stderr="",
        )
        with mock.patch(
            "whisper_dictate.vp_devices.subprocess.run", return_value=completed
        ) as run:
            result = vp_devices.list_input_devices(sd)
        run.assert_called_once()
        args, kwargs = run.call_args
        self.assertEqual(args[0], ["/fake/whisper-dictate", "devices"])
        # Always pipes a JSON request body so the binary can pick the action.
        self.assertEqual(json.loads(kwargs["input"]), {"action": "list"})
        self.assertFalse(kwargs.get("shell"))
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["name"], "Headset Microphone (Jabra Evolve 65 TE)")
        self.assertTrue(result[0]["default"])
        self.assertEqual(result[1]["max_input_channels"], 2)

    # --- failure modes fall back -------------------------------------------

    def test_helper_nonzero_exit_falls_back_to_python(self):
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        sd = _fake_sd(
            devices=[{"name": "Mic", "max_input_channels": 1, "hostapi": 0}],
            default_device=0,
            hostapis=[{"name": "ALSA", "default_input_device": 0}],
        )
        completed = types.SimpleNamespace(
            returncode=2,
            stdout='{"error":"devices_unavailable"}',
            stderr="",
        )
        with mock.patch(
            "whisper_dictate.vp_devices.subprocess.run", return_value=completed
        ):
            result = vp_devices.list_input_devices(sd)
        self.assertEqual(result[0]["name"], "Mic")

    def test_helper_invalid_json_falls_back(self):
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        sd = _fake_sd(
            devices=[{"name": "Mic", "max_input_channels": 1, "hostapi": 0}],
            default_device=0,
            hostapis=[{"name": "ALSA", "default_input_device": 0}],
        )
        completed = types.SimpleNamespace(
            returncode=0,
            stdout="not json",
            stderr="",
        )
        with mock.patch(
            "whisper_dictate.vp_devices.subprocess.run", return_value=completed
        ):
            result = vp_devices.list_input_devices(sd)
        self.assertEqual(result[0]["name"], "Mic")

    def test_helper_subprocess_exception_falls_back(self):
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        sd = _fake_sd(
            devices=[{"name": "Mic", "max_input_channels": 1, "hostapi": 0}],
            default_device=0,
            hostapis=[{"name": "ALSA", "default_input_device": 0}],
        )
        with mock.patch(
            "whisper_dictate.vp_devices.subprocess.run",
            side_effect=OSError("boom"),
        ):
            result = vp_devices.list_input_devices(sd)
        self.assertEqual(result[0]["name"], "Mic")

    def test_helper_filters_zero_channel_entries(self):
        # Defensive: the Rust binary already filters these, but vp_devices
        # must not leak a zero-channel entry into the picker if a future
        # binary regresses.
        os.environ["VOICEPI_DEVICES_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        rust_payload = {
            "devices": [
                {"index": 0, "name": "Speakers Only", "max_input_channels": 0,
                 "sample_rates": [48000, 48000], "default": False},
                {"index": 1, "name": "Real Mic", "max_input_channels": 1,
                 "sample_rates": [16000, 48000], "default": True},
            ]
        }
        completed = types.SimpleNamespace(
            returncode=0, stdout=json.dumps(rust_payload), stderr="",
        )
        sd = types.SimpleNamespace(
            query_devices=mock.Mock(side_effect=AssertionError("python path not expected")),
            query_hostapis=mock.Mock(),
            default=types.SimpleNamespace(device=None),
        )
        with mock.patch(
            "whisper_dictate.vp_devices.subprocess.run", return_value=completed
        ):
            result = vp_devices.list_input_devices(sd)
        self.assertEqual([d["name"] for d in result], ["Real Mic"])


if __name__ == "__main__":
    unittest.main()
