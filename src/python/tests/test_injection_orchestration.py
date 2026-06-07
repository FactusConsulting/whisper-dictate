"""End-to-end INPUT/OUTPUT injection-orchestration tests for InjectMixin.

test_injection_keymap.py covers strategy *selection* and the Wayland shortcut
byte-codes. These tests drive the actual injection ORCHESTRATION with the OS
backends stubbed so the typed/pasted text is captured and asserted:

  - _inject dispatch end to end: print vs type vs paste, on X11/Windows and
    Wayland, asserting the text really reached the (stubbed) keyboard/clipboard.
  - _paste internals (NOT stubbed): pyperclip.copy + the Ctrl+V key sequence
    via a fake pynput Controller, the Wayland ydotool branch, and the
    paste-failure path.
  - focus/target capture + restore (_capture_target_window on Windows via a
    fake ctypes, _restore_target_focus via xdotool).
  - fallbacks: paste falling back to direct typing when _paste fails, and
    Wayland ydotool typing falling back to pynput.

A fake pynput keyboard module is installed so `from pynput import keyboard`
inside _paste resolves to controllable Key sentinels and a recording
Controller. A real Dictate-shaped object is never needed — InjectMixin
methods only read `self.` attributes we set explicitly.
"""
import types as _types

from helpers import (
    _capture_stdout,
    _env,
    patch,
    sys,
    types,
    unittest,
)


def _fake_pynput_keyboard():
    """A pynput.keyboard stand-in: Key sentinels + a recording Controller."""
    keyboard = _types.ModuleType("keyboard")
    keyboard.Key = types.SimpleNamespace(
        ctrl="<ctrl>", ctrl_l="<ctrl_l>", ctrl_r="<ctrl_r>",
        shift="<shift>", shift_l="<shift_l>", shift_r="<shift_r>",
        alt="<alt>", alt_l="<alt_l>", alt_r="<alt_r>",
        cmd="<cmd>", cmd_l="<cmd_l>", cmd_r="<cmd_r>",
    )
    return keyboard


class _RecordingKb:
    """Fake pynput Controller: records press/release/type calls in order."""

    def __init__(self):
        self.events = []

    def press(self, k):
        self.events.append(("press", k))

    def release(self, k):
        self.events.append(("release", k))

    def type(self, text):
        self.events.append(("type", text))


class _FakeClip:
    def __init__(self):
        self.copied = []

    def copy(self, text):
        self.copied.append(text)


class _InjectBase(unittest.TestCase):
    def setUp(self):
        from whisper_dictate import vp_inject
        self.inject = vp_inject
        self.kbmod = _fake_pynput_keyboard()
        self.clip = _FakeClip()
        # `from pynput import keyboard` and `import pyperclip` happen inside
        # the methods under test; route both to our fakes.
        pynput = _types.ModuleType("pynput")
        pynput.keyboard = self.kbmod
        self._modpatch = patch.dict(sys.modules, {
            "pynput": pynput,
            "pynput.keyboard": self.kbmod,
            "pyperclip": self.clip,
        })
        self._modpatch.start()
        self.addCleanup(self._modpatch.stop)

    def _target(self, mode="auto", title=None, process=None, **extra):
        t = types.SimpleNamespace(
            mode=mode,
            _kb=_RecordingKb(),
            _inject_target_xwin=None,
            _inject_target_title=title,
            _inject_target_process=process,
            _xkb_layout="",
            _last_inject_strategy=None,
        )
        # Bind the real InjectMixin predicate methods used inside _inject so the
        # dispatch decisions (self-target, paste-preference) are exercised for
        # real rather than re-stated by the test. These read only the
        # attributes set above.
        mixin = self.inject.InjectMixin
        for name in (
            "_target_is_self",
            "_target_prefers_paste",
            "_text_prefers_paste",
            "_wayland_text_prefers_paste",
            "_wayland_target_prefers_terminal_paste",
            "_paste",
        ):
            method = getattr(mixin, name)
            setattr(t, name, method.__get__(t, type(t)))
        for k, v in extra.items():
            setattr(t, k, v)
        return t


class InjectDispatchTests(_InjectBase):
    def test_print_mode_emits_heard_line_and_does_not_type(self):
        t = self._target(mode="print")

        with _env(WAYLAND_DISPLAY=None), _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "hello world")

        self.assertEqual(t._last_inject_strategy, "print")
        self.assertIn("(heard) hello world", out.getvalue())
        self.assertEqual(t._kb.events, [])
        self.assertEqual(self.clip.copied, [])

    def test_x11_type_mode_types_text_via_controller(self):
        t = self._target(mode="type")

        with _env(WAYLAND_DISPLAY=None), \
                patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value=None), \
                _capture_stdout():
            self.inject.InjectMixin._inject(t, "typed text")

        self.assertEqual(t._last_inject_strategy, "type")
        self.assertEqual(t._kb.events, [("type", "typed text")])
        self.assertEqual(self.clip.copied, [])

    def test_x11_paste_mode_copies_and_sends_ctrl_v(self):
        t = self._target(mode="paste")

        with _env(WAYLAND_DISPLAY=None), \
                patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value=None), \
                _capture_stdout():
            self.inject.InjectMixin._inject(t, "pasted text")

        self.assertEqual(t._last_inject_strategy, "paste")
        # _paste ran for real: clipboard set, then a Ctrl+V key sequence.
        self.assertEqual(self.clip.copied, ["pasted text"])
        self.assertIn(("press", self.kbmod.Key.ctrl), t._kb.events)
        self.assertIn(("press", "v"), t._kb.events)
        self.assertIn(("release", "v"), t._kb.events)
        self.assertIn(("release", self.kbmod.Key.ctrl), t._kb.events)
        # No direct typing happened on the success path.
        self.assertNotIn(("type", "pasted text"), t._kb.events)

    def test_x11_paste_falls_back_to_typing_when_clipboard_unavailable(self):
        t = self._target(mode="paste")
        # pyperclip raising simulates "no clipboard backend"; _paste returns
        # False and _inject falls back to direct typing.
        boom = _types.ModuleType("pyperclip")

        def _raise(_text):
            raise RuntimeError("no clipboard")

        boom.copy = _raise

        with _env(WAYLAND_DISPLAY=None), \
                patch.dict(sys.modules, {"pyperclip": boom}), \
                patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value=None), \
                _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "fallback text")

        self.assertEqual(t._last_inject_strategy, "type-fallback")
        self.assertEqual(t._kb.events, [("type", "fallback text")])
        self.assertIn("paste fejlede", out.getvalue())

    def test_x11_auto_types_plain_text(self):
        t = self._target(mode="auto", title="Untitled - Notepad", process="notepad.exe")

        # Non-Windows + non-terminal target + ASCII text -> auto chooses type.
        with _env(WAYLAND_DISPLAY=None), \
                patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value=None), \
                _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "plain ascii")

        self.assertEqual(t._last_inject_strategy, "type")
        self.assertEqual(t._kb.events, [("type", "plain ascii")])
        self.assertIn("strategy: type", out.getvalue())

    def test_windows_auto_pastes_for_terminal_target(self):
        t = self._target(
            mode="auto",
            title="Administrator: Windows PowerShell",
            process="WindowsTerminal.exe",
        )

        with _env(WAYLAND_DISPLAY=None), \
                patch.object(self.inject.os, "name", "nt"), \
                _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "dir")

        self.assertEqual(t._last_inject_strategy, "paste")
        self.assertEqual(self.clip.copied, ["dir"])
        self.assertIn("strategy: paste", out.getvalue())

    def test_windows_auto_pastes_layout_sensitive_text_in_plain_app(self):
        t = self._target(mode="auto", title="Untitled - Notepad", process="notepad.exe")

        # An apostrophe is layout-sensitive on Windows -> auto switches to paste.
        with _env(WAYLAND_DISPLAY=None), \
                patch.object(self.inject.os, "name", "nt"), \
                _capture_stdout():
            self.inject.InjectMixin._inject(t, "I'm here")

        self.assertEqual(t._last_inject_strategy, "paste")
        self.assertEqual(self.clip.copied, ["I'm here"])


class InjectWaylandDispatchTests(_InjectBase):
    """Wayland path end-to-end with the rust injector / ydotool / pynput
    boundaries stubbed on the instance (mirrors the real call surface)."""

    def _wl_target(self, mode="auto", **extra):
        t = self._target(mode=mode, **extra)
        t.pasted = []
        t.typed_wayland = []

        def _paste(text):
            t.pasted.append(text)
            return extra.get("paste_ok", True)

        def _wayland_type(text):
            t.typed_wayland.append(text)
            return extra.get("wayland_ok", True)

        t._paste = _paste
        t._wayland_type = _wayland_type
        t._restore_target_focus = lambda: False
        return t

    def test_wayland_auto_pastes_non_ascii(self):
        t = self._wl_target(mode="auto")

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout():
            self.inject.InjectMixin._inject(t, "ør边")

        self.assertEqual(t._last_inject_strategy, "paste")
        self.assertEqual(t.pasted, ["ør边"])
        self.assertEqual(t.typed_wayland, [])

    def test_wayland_auto_types_ascii_via_ydotool(self):
        t = self._wl_target(mode="auto")

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout():
            self.inject.InjectMixin._inject(t, "plain ascii")

        self.assertEqual(t._last_inject_strategy, "ydotool")
        self.assertEqual(t.typed_wayland, ["plain ascii"])
        self.assertEqual(t.pasted, [])

    def test_wayland_ydotool_failure_falls_back_to_pynput_type(self):
        t = self._wl_target(mode="type", wayland_ok=False)

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "plain ascii")

        # _wayland_type returned False -> direct pynput type fallback.
        self.assertEqual(t._last_inject_strategy, "type-fallback")
        self.assertEqual(t._kb.events, [("type", "plain ascii")])
        self.assertIn("fallback pynput", out.getvalue())

    def test_wayland_paste_failure_falls_back_to_ydotool(self):
        t = self._wl_target(mode="auto", paste_ok=False)

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "øre")

        # paste chosen for non-ASCII, fails, then ydotool typing is attempted.
        self.assertEqual(t._last_inject_strategy, "ydotool")
        self.assertEqual(t.pasted, ["øre"])
        self.assertEqual(t.typed_wayland, ["øre"])
        self.assertIn("fallback ydotool", out.getvalue())


class PasteInternalsTests(_InjectBase):
    """Drive _paste directly (NOT stubbed) to pin the clipboard + key sequence."""

    def test_paste_releases_modifiers_before_ctrl_v(self):
        t = self._target()

        with _env(WAYLAND_DISPLAY=None):
            ok = self.inject.InjectMixin._paste(t, "clip me")

        self.assertTrue(ok)
        self.assertEqual(self.clip.copied, ["clip me"])
        names = [e for e in t._kb.events]
        # Stale modifiers are released first (best-effort), then Ctrl+V.
        self.assertIn(("release", self.kbmod.Key.shift), names)
        self.assertIn(("release", self.kbmod.Key.alt), names)
        # The final four events are the deterministic Ctrl+V chord.
        self.assertEqual(
            names[-4:],
            [
                ("press", self.kbmod.Key.ctrl),
                ("press", "v"),
                ("release", "v"),
                ("release", self.kbmod.Key.ctrl),
            ],
        )

    def test_paste_uses_wayland_shortcut_and_skips_pynput(self):
        called = {"shortcut": 0}

        t = self._target()
        t._wayland_paste_shortcut = lambda: (called.__setitem__(
            "shortcut", called["shortcut"] + 1) or True)

        with _env(WAYLAND_DISPLAY="wayland-0"):
            ok = self.inject.InjectMixin._paste(t, "wl clip")

        self.assertTrue(ok)
        self.assertEqual(self.clip.copied, ["wl clip"])
        self.assertEqual(called["shortcut"], 1)
        # Wayland shortcut handled it; no pynput key events.
        self.assertEqual(t._kb.events, [])

    def test_paste_on_wayland_falls_through_to_pynput_when_shortcut_fails(self):
        t = self._target()
        t._wayland_paste_shortcut = lambda: False

        with _env(WAYLAND_DISPLAY="wayland-0"):
            ok = self.inject.InjectMixin._paste(t, "wl clip")

        self.assertTrue(ok)
        # Shortcut failed -> falls through to the pynput Ctrl+V chord.
        self.assertEqual(t._kb.events[-4:], [
            ("press", self.kbmod.Key.ctrl),
            ("press", "v"),
            ("release", "v"),
            ("release", self.kbmod.Key.ctrl),
        ])

    def test_paste_returns_false_and_logs_when_clipboard_raises(self):
        t = self._target()
        boom = _types.ModuleType("pyperclip")

        def _raise(_text):
            raise RuntimeError("xclip missing")

        boom.copy = _raise

        with _env(WAYLAND_DISPLAY=None), \
                patch.dict(sys.modules, {"pyperclip": boom}), \
                _capture_stdout() as out:
            ok = self.inject.InjectMixin._paste(t, "nope")

        self.assertFalse(ok)
        self.assertIn("paste fejlede", out.getvalue())
        self.assertEqual(t._kb.events, [])


class FocusCaptureRestoreTests(_InjectBase):
    def test_capture_windows_target_reads_title_and_process(self):
        # t is a SimpleNamespace (not an InjectMixin instance), so the helper
        # _capture_windows_target calls must be bound onto t directly rather
        # than patched on the class.
        t = types.SimpleNamespace(
            _windows_process_name=lambda c, w, pid: "myeditor.exe",
        )

        # Fake ctypes user32/kernel32 returning a known window + process.
        class _Buf:
            def __init__(self, value=""):
                self.value = value

        def create_unicode_buffer(arg):
            # GetWindowTextW path passes a length (int); name buffer passes size.
            return _Buf("")

        user32 = types.SimpleNamespace(
            GetForegroundWindow=lambda: 4242,
            GetWindowTextLengthW=lambda hwnd: 5,
        )

        def get_window_text(hwnd, buf, n):
            buf.value = "My Editor"
            return len("My Editor")

        user32.GetWindowTextW = get_window_text

        def get_thread_pid(hwnd, byref_pid):
            byref_pid._obj.value = 9999
            return 1

        user32.GetWindowThreadProcessId = get_thread_pid

        class _DWORD:
            def __init__(self, value=0):
                self.value = value

        wintypes = types.SimpleNamespace(DWORD=_DWORD)

        fake_ctypes = types.SimpleNamespace(
            windll=types.SimpleNamespace(user32=user32),
            create_unicode_buffer=create_unicode_buffer,
            byref=lambda obj: types.SimpleNamespace(_obj=obj),
        )

        # ctypes.wintypes is imported as `from ctypes import wintypes`.
        fake_ctypes.wintypes = wintypes
        with patch.dict(sys.modules, {
                "ctypes": fake_ctypes, "ctypes.wintypes": wintypes}):
            self.inject.InjectMixin._capture_windows_target(t)

        self.assertEqual(t._inject_target_title, "My Editor")
        self.assertEqual(t._inject_target_process, "myeditor.exe")

    def test_capture_target_window_resets_state_when_no_xdotool(self):
        t = types.SimpleNamespace(
            _inject_target_xwin="stale",
            _inject_target_title="stale",
            _inject_target_process="stale",
        )

        with patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value=None):
            self.inject.InjectMixin._capture_target_window(t)

        self.assertIsNone(t._inject_target_xwin)
        self.assertIsNone(t._inject_target_title)
        self.assertIsNone(t._inject_target_process)

    def test_capture_target_window_reads_xdotool_active_window(self):
        t = types.SimpleNamespace()
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(cmd)
            if cmd[:2] == ["xdotool", "getactivewindow"]:
                return self.inject.subprocess.CompletedProcess(cmd, 0, stdout=b"12345\n")
            if cmd[:2] == ["xdotool", "getwindowname"]:
                return self.inject.subprocess.CompletedProcess(cmd, 0, stdout=b"Cool App\n")
            return self.inject.subprocess.CompletedProcess(cmd, 1, stdout=b"")

        with patch.object(self.inject.os, "name", "posix"), \
                patch.object(self.inject.shutil, "which", return_value="/usr/bin/xdotool"), \
                patch.object(self.inject.subprocess, "run", fake_run):
            self.inject.InjectMixin._capture_target_window(t)

        self.assertEqual(t._inject_target_xwin, "12345")
        self.assertEqual(t._inject_target_title, "Cool App")

    def test_restore_target_focus_activates_known_window(self):
        t = types.SimpleNamespace(
            _inject_target_xwin="12345",
            _inject_target_title="Cool App",
        )
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(cmd)
            return self.inject.subprocess.CompletedProcess(cmd, 0)

        with patch.object(self.inject.shutil, "which", return_value="/usr/bin/xdotool"), \
                patch.object(self.inject.subprocess, "run", fake_run):
            ok = self.inject.InjectMixin._restore_target_focus(t)

        self.assertTrue(ok)
        self.assertEqual(
            calls[0],
            ["xdotool", "windowactivate", "--sync", "12345"],
        )

    def test_restore_target_focus_skips_when_title_unknown(self):
        # Wayland-native: xdotool finds an XID but no title -> refocus skipped.
        t = types.SimpleNamespace(
            _inject_target_xwin="12345",
            _inject_target_title=None,
        )

        with patch.object(self.inject.shutil, "which", return_value="/usr/bin/xdotool"):
            self.assertFalse(self.inject.InjectMixin._restore_target_focus(t))

    def test_inject_print_mode_skips_refocus_log(self):
        t = self._target(mode="print", title="Cool App")
        t._inject_target_xwin = "12345"
        t._restore_target_focus = lambda: True

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "hi")

        # print mode short-circuits BEFORE the refocus log.
        self.assertEqual(t._last_inject_strategy, "print")
        self.assertIn("(heard) hi", out.getvalue())
        self.assertNotIn("refocused", out.getvalue())

    def test_inject_refocus_log_for_non_print_wayland(self):
        t = self._target(mode="type", title="Cool App")
        t._inject_target_xwin = "12345"
        t._restore_target_focus = lambda: True
        t._wayland_type = lambda text: True

        with _env(WAYLAND_DISPLAY="wayland-0"), _capture_stdout() as out:
            self.inject.InjectMixin._inject(t, "hi there")

        self.assertIn("refocused: Cool App", out.getvalue())


if __name__ == "__main__":
    unittest.main()
