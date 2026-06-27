"""Tests for vp_feedback — audible cues and desktop notifications.

Coverage:
  * play_cue: env off → no calls; env on → platform-correct call.
  * notify_error: env off → no Popen; env on → notify-send called on Linux;
    no-op on Windows/other; exception is swallowed.
  * Integration: vp_dictate._start / _stop_and_transcribe call play_cue.
"""
from __future__ import annotations

import sys
import types
import unittest
from unittest.mock import MagicMock, patch, call

from helpers import _env, _capture_stdout


# ---------------------------------------------------------------------------
# Helpers: fresh import of vp_feedback after evicting cached module
# ---------------------------------------------------------------------------

def _import_feedback():
    """Re-import vp_feedback with a clean slate so env changes take effect."""
    for mod in list(sys.modules.keys()):
        if "vp_feedback" in mod or "vp_config" in mod:
            sys.modules.pop(mod, None)
    # Ensure the package root is on sys.path (mirrors helpers.py)
    import os
    from pathlib import Path
    src = str(Path(__file__).resolve().parents[1])
    if src not in sys.path:
        sys.path.insert(0, src)
    import importlib
    return importlib.import_module("whisper_dictate.vp_feedback")


# ---------------------------------------------------------------------------
# play_cue — env off
# ---------------------------------------------------------------------------

class PlayCueOffTests(unittest.TestCase):
    def test_sounds_off_no_winsound_call(self):
        fb = _import_feedback()
        mock_winsound = MagicMock()
        with _env(VOICEPI_FEEDBACK_SOUNDS="0"), \
                patch.dict(sys.modules, {"winsound": mock_winsound}):
            fb.play_cue("start")
            fb.play_cue("stop")
        mock_winsound.Beep.assert_not_called()
        mock_winsound.MessageBeep.assert_not_called()

    def test_sounds_off_no_popen_call(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_SOUNDS=""), \
                patch("subprocess.Popen") as mock_popen:
            fb.play_cue("start")
            fb.play_cue("stop")
        mock_popen.assert_not_called()


# ---------------------------------------------------------------------------
# play_cue — Windows path
# ---------------------------------------------------------------------------

class _InlineThread:
    """Run the target synchronously — the Windows beep runs on a daemon thread
    (winsound.Beep is synchronous), so tests must not race the scheduler."""

    def __init__(self, target=None, daemon=None):
        self._target = target

    def start(self):
        if self._target is not None:
            self._target()


class PlayCueWindowsTests(unittest.TestCase):
    def test_start_plays_880hz_beep(self):
        fb = _import_feedback()
        mock_winsound = MagicMock()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch.dict(sys.modules, {"winsound": mock_winsound}), \
                patch.object(sys, "platform", "win32"), \
                patch("os.name", "nt"):
            fb.play_cue("start")
        mock_winsound.Beep.assert_called_once_with(880, 80)

    def test_stop_plays_440hz_beep(self):
        fb = _import_feedback()
        mock_winsound = MagicMock()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch.dict(sys.modules, {"winsound": mock_winsound}), \
                patch.object(sys, "platform", "win32"), \
                patch("os.name", "nt"):
            fb.play_cue("stop")
        mock_winsound.Beep.assert_called_once_with(440, 80)

    def test_winsound_exception_is_swallowed(self):
        fb = _import_feedback()
        mock_winsound = MagicMock()
        mock_winsound.Beep.side_effect = OSError("no audio device")
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch.dict(sys.modules, {"winsound": mock_winsound}), \
                patch("os.name", "nt"):
            # Must not raise
            fb.play_cue("start")


# ---------------------------------------------------------------------------
# play_cue — Linux path
# ---------------------------------------------------------------------------

class PlayCueLinuxTests(unittest.TestCase):
    def test_start_calls_paplay_with_message_sound(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch("os.name", "posix"), \
                patch.object(sys, "platform", "linux"), \
                patch("os.path.exists", return_value=True), \
                patch("subprocess.Popen") as mock_popen:
            fb.play_cue("start")
        mock_popen.assert_called_once()
        args = mock_popen.call_args[0][0]
        self.assertEqual(args[0], "paplay")
        self.assertIn("message", args[1])

    def test_stop_calls_paplay_with_dialog_sound(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch("os.name", "posix"), \
                patch.object(sys, "platform", "linux"), \
                patch("os.path.exists", return_value=True), \
                patch("subprocess.Popen") as mock_popen:
            fb.play_cue("stop")
        mock_popen.assert_called_once()
        args = mock_popen.call_args[0][0]
        self.assertEqual(args[0], "paplay")
        self.assertIn("dialog-information", args[1])

    def test_missing_sound_file_is_silent(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch("os.name", "posix"), \
                patch.object(sys, "platform", "linux"), \
                patch("os.path.exists", return_value=False), \
                patch("subprocess.Popen") as mock_popen:
            fb.play_cue("start")
        mock_popen.assert_not_called()

    def test_popen_exception_is_swallowed(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch("os.name", "posix"), \
                patch.object(sys, "platform", "linux"), \
                patch("os.path.exists", return_value=True), \
                patch("subprocess.Popen", side_effect=OSError("no paplay")):
            # Must not raise
            fb.play_cue("start")


# ---------------------------------------------------------------------------
# play_cue — macOS / other platforms are no-ops
# ---------------------------------------------------------------------------

class PlayCueMacOsTests(unittest.TestCase):
    def test_macos_no_popen_no_winsound(self):
        fb = _import_feedback()
        mock_winsound = MagicMock()
        with _env(VOICEPI_FEEDBACK_SOUNDS="1"), \
                patch.object(fb, "_sounds_enabled", return_value=True), \
                patch("os.name", "posix"), \
                patch.object(sys, "platform", "darwin"), \
                patch.dict(sys.modules, {"winsound": mock_winsound}), \
                patch("subprocess.Popen") as mock_popen:
            fb.play_cue("start")
        mock_popen.assert_not_called()
        mock_winsound.Beep.assert_not_called()


# ---------------------------------------------------------------------------
# notify_error — gating
# ---------------------------------------------------------------------------

class NotifyErrorOffTests(unittest.TestCase):
    def test_notify_off_no_popen(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_NOTIFY="0"), \
                patch("subprocess.Popen") as mock_popen:
            fb.notify_error("whisper-dictate", "test error")
        mock_popen.assert_not_called()

    def test_notify_unset_no_popen(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_NOTIFY=None), \
                patch("subprocess.Popen") as mock_popen:
            fb.notify_error("whisper-dictate", "test error")
        mock_popen.assert_not_called()


class NotifyErrorLinuxTests(unittest.TestCase):
    def test_notify_on_linux_calls_notify_send(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_NOTIFY="1"), \
                patch.object(fb, "_notify_enabled", return_value=True), \
                patch.object(sys, "platform", "linux"), \
                patch("subprocess.Popen") as mock_popen:
            fb.notify_error("whisper-dictate", "Model load failed: OOM")
        mock_popen.assert_called_once()
        args = mock_popen.call_args[0][0]
        self.assertIn("notify-send", args)
        self.assertIn("whisper-dictate", args)
        self.assertIn("Model load failed: OOM", args)
        self.assertIn("--urgency=critical", args)

    def test_notify_send_exception_is_swallowed(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_NOTIFY="1"), \
                patch.object(fb, "_notify_enabled", return_value=True), \
                patch.object(sys, "platform", "linux"), \
                patch("subprocess.Popen", side_effect=FileNotFoundError("notify-send not found")):
            # Must not raise
            fb.notify_error("whisper-dictate", "some error")


class NotifyErrorWindowsTests(unittest.TestCase):
    def test_notify_on_windows_is_noop(self):
        fb = _import_feedback()
        with _env(VOICEPI_FEEDBACK_NOTIFY="1"), \
                patch.object(fb, "_notify_enabled", return_value=True), \
                patch.object(sys, "platform", "win32"), \
                patch("os.name", "nt"), \
                patch("subprocess.Popen") as mock_popen:
            fb.notify_error("whisper-dictate", "some error")
        mock_popen.assert_not_called()


# ---------------------------------------------------------------------------
# Integration: vp_dictate hooks
# ---------------------------------------------------------------------------

class DictateHookTests(unittest.TestCase):
    """Verify that _start and _stop_and_transcribe call play_cue.

    Uses the same object.__new__ bypass pattern as test_dictate_loop.py to
    avoid importing the heavy runtime, patching just the vp_feedback module
    reference in vp_dictate.
    """

    @classmethod
    def setUpClass(cls):
        # Ensure heavy deps are stubbed before importing vp_dictate.
        for mod in ["ctranslate2", "faster_whisper", "sounddevice",
                    "pynput", "pynput.keyboard"]:
            if mod not in sys.modules:
                sys.modules[mod] = types.ModuleType(mod)

        kb = sys.modules.get("pynput.keyboard") or types.ModuleType("pynput.keyboard")
        kb.Controller = object
        kb.Key = types.SimpleNamespace(ctrl_l=object(), ctrl_r=object())
        kb.Listener = object
        sys.modules["pynput.keyboard"] = kb
        pynput = sys.modules.get("pynput") or types.ModuleType("pynput")
        pynput.keyboard = kb
        sys.modules["pynput"] = pynput

        import importlib
        import os
        from pathlib import Path
        src = str(Path(__file__).resolve().parents[1])
        if src not in sys.path:
            sys.path.insert(0, src)
        cls.vp_dictate = importlib.import_module("whisper_dictate.vp_dictate")

    def _make_dictate(self):
        """Build a minimal Dictate-like object bypassing __init__."""
        d = object.__new__(self.vp_dictate.Dictate)
        d.recording = False
        d.release_tail_ms = 0
        d._preview = None
        d._arecord_proc = None
        d._stream = None
        d.audio_ducker = types.SimpleNamespace(enter=lambda: None, exit=lambda: None)
        d._capture_backend = "test"
        d._audio_input_device = "test"
        d._capture_channels = 1
        d.frames = []
        d._record_started = 0.0
        d._record_keydown_at = 0.0
        d._first_audio_at = 0.0
        d._first_audio_event = __import__("threading").Event()
        d._first_audio_event.set()  # simulate audio already arrived
        d._last_audio_level_event = 0.0
        d.stt_backend = "whisper"
        d.preview_seconds = 0.0
        d.model = object()
        d.lang = "en"
        d.postprocess_settings = None
        d.model_name = "test-model"
        d.device = "cpu"
        d.compute_type = "int8"
        d.model_load_s = 0.0
        d.mode = "print"
        d._last_inject_strategy = None
        d._inject_target_title = None
        d._inject_target_process = None
        d._active_profile_name = None
        d._cap_warned = False
        # Stub boundary methods
        d._reload_live_config_if_changed = lambda: None
        d._capture_target_window = lambda: None
        d._profiled_config = lambda base: base
        d._apply_effective_config = lambda after: None
        d._effective_config = {}
        d._start_arecord = lambda: ("test", "test")
        d._start_sounddevice = lambda: ("test", "test")
        d._stop_capture_streams = lambda: None
        return d

    def test_start_calls_play_cue_start(self):
        vd = self.vp_dictate
        d = self._make_dictate()
        with patch.object(vd, "play_cue") as mock_cue, \
                _capture_stdout():
            vd.Dictate._start(d)
        mock_cue.assert_any_call("start")

    def test_stop_calls_play_cue_stop(self):
        vd = self.vp_dictate
        d = self._make_dictate()
        d.recording = True
        with patch.object(vd, "play_cue") as mock_cue, \
                _capture_stdout():
            vd.Dictate._stop_and_transcribe(d)
        mock_cue.assert_any_call("stop")


if __name__ == "__main__":
    unittest.main()
