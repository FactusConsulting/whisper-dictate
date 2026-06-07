"""Injection dispatch tests for InjectMixin (X11/Windows + Wayland).

test_injection_keymap.py covers strategy *selection*; these drive the actual
_inject ORCHESTRATION with the OS backends stubbed so the typed/pasted text is
captured and asserted:

  - _inject dispatch end to end: print vs type vs paste, on X11/Windows and
    Wayland, asserting the text really reached the (stubbed) keyboard/clipboard.

Paste internals and focus capture/restore live in test_injection_paste.py.

A fake pynput keyboard module is installed so `from pynput import keyboard`
resolves to controllable Key sentinels and a recording Controller. A real
Dictate-shaped object is never needed — InjectMixin methods only read `self.`
attributes we set explicitly.
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


