"""Paste-internals + focus capture/restore tests for InjectMixin.

Companion to test_injection_dispatch.py (which covers _inject dispatch); these
drive the lower-level paste + focus paths with the OS backends stubbed:

  - _paste internals (NOT stubbed): pyperclip.copy + the Ctrl+V key sequence
    via a fake pynput Controller, the Wayland ydotool branch, and the
    paste-failure path.
  - clipboard save/restore: saves before copy, restores after delay (only if
    clipboard unchanged), skips restore when clipboard changed in between,
    survives pyperclip exceptions.
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
    def __init__(self, initial: str = ""):
        self.copied = []
        self._current = initial

    def copy(self, text: str) -> None:
        self.copied.append(text)
        self._current = text

    def paste(self) -> str:
        return self._current


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
            # _inject delegates its per-platform body to these; bind the real
            # ones so the dispatch is exercised end-to-end on the namespace.
            "_inject_log_preview",
            "_inject_wayland",
            "_inject_other",
        ):
            method = getattr(mixin, name)
            setattr(t, name, method.__get__(t, type(t)))
        for k, v in extra.items():
            setattr(t, k, v)
        return t


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


class ClipboardRestoreTests(_InjectBase):
    """Clipboard save-and-restore logic in _paste (delay and guard tests).

    These tests disable the background thread via the module flag and instead
    invoke the internal restore closure synchronously, so they are
    deterministic and thread-free.
    """

    def setUp(self):
        super().setUp()
        # Replace the fake clip with one that has an initial value.
        self.clip = _FakeClip(initial="previous content")
        pynput = _types.ModuleType("pynput")
        pynput.keyboard = self.kbmod
        # Re-patch with the updated clip (includes paste()).
        self._modpatch.stop()
        self._modpatch = patch.dict(sys.modules, {
            "pynput": pynput,
            "pynput.keyboard": self.kbmod,
            "pyperclip": self.clip,
        })
        self._modpatch.start()

    def _run_paste_sync(self, text: str, *, restore_enabled: bool = True):
        """Run _paste with the background thread disabled; return (ok, restore_fn).

        The restore thread is replaced with a direct call holder so tests can
        trigger the restore synchronously without sleeping.
        """
        from whisper_dictate import vp_inject
        restore_calls = []

        import threading as _threading

        class _CapturingThread:
            """Capture the restore target callable instead of spawning a thread."""
            def __init__(self, target=None, args=(), daemon=None):
                self._target = target
                self._args = tuple(args)

            def start(self):
                if self._target is not None:
                    target, args = self._target, self._args
                    restore_calls.append(lambda: target(*args))

        with patch.object(vp_inject, "_CLIPBOARD_RESTORE_ENABLED", restore_enabled), \
                patch.object(
                    sys.modules.get("threading") or _threading,
                    "Thread",
                    _CapturingThread,
                ):
            t = self._target()
            with _env(WAYLAND_DISPLAY=None):
                ok = vp_inject.InjectMixin._paste(t, text)
        return ok, restore_calls

    def test_clipboard_is_saved_before_copy_and_restore_scheduled(self):
        ok, restore_calls = self._run_paste_sync("injected")

        self.assertTrue(ok)
        # Our text was written to clipboard.
        self.assertIn("injected", self.clip.copied)
        # A restore callback was scheduled.
        self.assertEqual(len(restore_calls), 1)

    def test_restore_reverts_clipboard_when_unchanged(self):
        """Restore fn writes previous content when clipboard still holds injected text."""
        from whisper_dictate import vp_inject
        _ok, restore_calls = self._run_paste_sync("injected text")

        # At this point clipboard == "injected text" (set by copy).
        self.assertEqual(self.clip._current, "injected text")

        # Run restore synchronously (bypass the sleep).
        restore_fn = restore_calls[0]
        with patch.object(vp_inject.time, "sleep", lambda _s: None):
            restore_fn()

        # Clipboard restored to previous value.
        self.assertEqual(self.clip._current, "previous content")

    def test_restore_skipped_when_clipboard_changed_in_between(self):
        """If the user copied something else, restore must NOT clobber it."""
        from whisper_dictate import vp_inject
        _ok, restore_calls = self._run_paste_sync("injected text")

        # Simulate user copying something in between.
        self.clip.copy("user copied this")

        restore_fn = restore_calls[0]
        with patch.object(vp_inject.time, "sleep", lambda _s: None):
            restore_fn()

        # Clipboard must stay with the user's content.
        self.assertEqual(self.clip._current, "user copied this")

    def test_restore_returns_when_clipboard_unreadable(self):
        """paste() raising during restore leaves the clipboard untouched."""
        from whisper_dictate import vp_inject

        class _Boom:
            def paste(self):
                raise RuntimeError("clipboard locked")

            def copy(self, _value):  # pragma: no cover - must not be reached
                raise AssertionError("copy must not run when paste() fails")

        # Direct call with delay 0 — covers the unreadable-clipboard return arm.
        vp_inject._restore_clipboard_after_delay(_Boom(), "injected", "prev", delay_s=0)

    def test_restore_swallows_copy_exception(self):
        """copy() raising during restore is swallowed (injection already done)."""
        from whisper_dictate import vp_inject

        class _CopyBoom:
            def paste(self):
                return "injected"

            def copy(self, _value):
                raise RuntimeError("clipboard write blocked")

        vp_inject._restore_clipboard_after_delay(
            _CopyBoom(), "injected", "prev", delay_s=0)

    def test_restore_swallows_unexpected_outer_exception(self):
        """Even a failing sleep must never propagate out of the restore thread."""
        from whisper_dictate import vp_inject

        with patch.object(vp_inject.time, "sleep",
                          side_effect=RuntimeError("interrupted")):
            vp_inject._restore_clipboard_after_delay(self.clip, "injected", "prev")

    def test_restore_skipped_when_previous_was_none(self):
        """No restore thread is started when pyperclip.paste() raises (nothing saved)."""
        boom = _types.ModuleType("pyperclip")

        def _raise():
            raise RuntimeError("no clipboard")

        boom.paste = _raise
        boom.copy = self.clip.copy

        with patch.dict(sys.modules, {"pyperclip": boom}):
            _ok, restore_calls = self._run_paste_sync("injected text")

        # paste() raised — nothing to restore, so no thread started.
        self.assertEqual(restore_calls, [])

    def test_paste_survives_pyperclip_paste_exception(self):
        """pyperclip.paste() raising must never break the injection itself."""
        boom = _types.ModuleType("pyperclip")

        def _raise():
            raise RuntimeError("no clipboard")

        boom.paste = _raise
        boom.copy = self.clip.copy

        with patch.dict(sys.modules, {"pyperclip": boom}), \
                _env(WAYLAND_DISPLAY=None):
            t = self._target()
            ok = self.inject.InjectMixin._paste(t, "text")

        self.assertTrue(ok)


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
