"""Tests for the window-listing feature (--list-windows worker flag).

Covers:
  - list_visible_windows() with a stubbed ctypes (mirrors how
    test_injection_paste.py stubs ctypes for capture tests).
  - Self-window filtering (_is_self_window + SELF_INJECTION_* constants).
  - print_windows() JSON output + non-Windows error path.
  - --list-windows flag wiring in vp_cli.build_arg_parser.
"""
from __future__ import annotations

import types as _types

from helpers import (
    _capture_stdout,
    json,
    patch,
    sys,
    unittest,
)


# ---------------------------------------------------------------------------
# Ctypes stub helpers (pattern from test_injection_paste.py)
# ---------------------------------------------------------------------------

def _fake_ctypes(windows: list[dict]):
    """Return (fake_ctypes, fake_wintypes) that simulate EnumWindows over
    *windows*, a list of dicts with keys:
      - "title": str          window title (empty → IsWindowVisible returns 0)
      - "visible": bool       (default True)
      - "pid": int            (default 1)
      - "exe": str            full exe path (default "C:\\notepad.exe")
    """
    ctypes_mod = _types.ModuleType("ctypes")
    wintypes_mod = _types.ModuleType("wintypes")

    class DWORD:
        def __init__(self, v=0):
            self.value = v

    wintypes_mod.DWORD = DWORD

    _callbacks: list = []

    class _Buffer:
        def __init__(self, text: str):
            self.value = text

    def create_unicode_buffer(n):
        return _Buffer("")

    def byref(obj):
        return obj

    def WINFUNCTYPE(*_args):
        def _decorator(fn):
            return fn
        return _decorator

    class _User32:
        def IsWindowVisible(self, hwnd):
            w = windows[hwnd]
            return 1 if w.get("visible", True) else 0

        def GetWindowTextLengthW(self, hwnd):
            return len(windows[hwnd].get("title", ""))

        def GetWindowTextW(self, hwnd, buf, _n):
            buf.value = windows[hwnd].get("title", "")

        def GetWindowThreadProcessId(self, hwnd, pid_byref):
            pid_byref.value = windows[hwnd].get("pid", 1)

        def EnumWindows(self, callback, lparam):
            for hwnd in range(len(windows)):
                if not callback(hwnd, lparam):
                    break

    # kernel32 stub: return each window's pid as the OpenProcess "handle" so
    # QueryFullProcessImageNameW can look the exe up by pid.
    pid_to_exe = {w.get("pid", 1): w.get("exe", "C:\\notepad.exe") for w in windows}

    class _Kernel32:
        def OpenProcess(self, _access, _inherit, pid):
            return pid  # return pid as the "handle" so we can look it up

        def QueryFullProcessImageNameW(self, handle, _flags, buf, _size):
            buf.value = pid_to_exe.get(handle, "C:\\unknown.exe")
            return 1

        def CloseHandle(self, _h):
            pass

    class _Windll:
        user32 = _User32()
        kernel32 = _Kernel32()

    ctypes_mod.windll = _Windll()
    ctypes_mod.c_bool = bool
    ctypes_mod.c_void_p = int
    ctypes_mod.c_long = int
    ctypes_mod.create_unicode_buffer = create_unicode_buffer
    ctypes_mod.byref = byref
    ctypes_mod.WINFUNCTYPE = WINFUNCTYPE

    return ctypes_mod, wintypes_mod


# ---------------------------------------------------------------------------
# list_visible_windows tests
# ---------------------------------------------------------------------------

class ListVisibleWindowsTests(unittest.TestCase):
    def test_returns_visible_windows_with_title_and_process(self):
        windows_spec = [
            {"title": "Notepad", "pid": 10, "exe": "C:\\Windows\\notepad.exe"},
            {"title": "Visual Studio Code", "pid": 20, "exe": "C:\\code.exe"},
        ]
        result = _run_list_with_stubs(windows_spec)
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["title"], "Notepad")
        self.assertEqual(result[0]["process"], "notepad.exe")

    def test_skips_invisible_windows(self):
        windows_spec = [
            {"title": "Visible Window", "pid": 1, "exe": "C:\\app.exe"},
            {"title": "Hidden Window", "visible": False, "pid": 2, "exe": "C:\\hidden.exe"},
        ]
        result = _run_list_with_stubs(windows_spec)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]["title"], "Visible Window")

    def test_skips_empty_titles(self):
        windows_spec = [
            {"title": "", "pid": 1, "exe": "C:\\app.exe"},
            {"title": "Real Window", "pid": 2, "exe": "C:\\app.exe"},
        ]
        result = _run_list_with_stubs(windows_spec)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]["title"], "Real Window")

    def test_filters_self_by_title(self):
        windows_spec = [
            {"title": "whisper-dictate 1.8.5", "pid": 99, "exe": "C:\\whisper-dictate.exe"},
            {"title": "Notepad", "pid": 1, "exe": "C:\\notepad.exe"},
        ]
        result = _run_list_with_stubs(windows_spec)
        titles = [w["title"] for w in result]
        self.assertNotIn("whisper-dictate 1.8.5", titles)
        self.assertIn("Notepad", titles)

    def test_filters_self_by_process_name(self):
        windows_spec = [
            {"title": "My App", "pid": 99, "exe": "C:\\whisper-dictate.exe"},
            {"title": "Notepad", "pid": 1, "exe": "C:\\notepad.exe"},
        ]
        result = _run_list_with_stubs(windows_spec)
        titles = [w["title"] for w in result]
        self.assertNotIn("My App", titles)
        self.assertIn("Notepad", titles)

    def test_non_windows_raises(self):
        from whisper_dictate import vp_windows
        with patch.object(vp_windows.os, "name", "posix"):
            with self.assertRaises(RuntimeError) as ctx:
                vp_windows.list_visible_windows()
        self.assertIn("Windows", str(ctx.exception))


def _run_list_with_stubs(windows_spec: list[dict]) -> list[dict]:
    """Call the REAL ``vp_windows.list_visible_windows`` under a stubbed
    ``ctypes`` / ``ctypes.wintypes`` and ``os.name == "nt"``.

    The production function does ``import ctypes`` / ``from ctypes import
    wintypes`` at call time, so injecting both into ``sys.modules`` makes the
    real callback logic run against our fakes. Crucially this exercises
    production code: if ``list_visible_windows`` is broken, these tests fail.
    """
    import types as _builtin_types
    from whisper_dictate import vp_windows

    ctypes_mod, wintypes_mod = _fake_ctypes(windows_spec)

    # `from ctypes import wintypes` resolves to the "ctypes.wintypes" submodule.
    fake_ctypes_wintypes = _builtin_types.ModuleType("ctypes.wintypes")
    fake_ctypes_wintypes.DWORD = wintypes_mod.DWORD
    ctypes_mod.wintypes = fake_ctypes_wintypes

    with patch.dict(sys.modules, {
        "ctypes": ctypes_mod,
        "ctypes.wintypes": fake_ctypes_wintypes,
    }):
        with patch.object(vp_windows.os, "name", "nt"):
            return vp_windows.list_visible_windows()


# ---------------------------------------------------------------------------
# print_windows tests
# ---------------------------------------------------------------------------

class PrintWindowsTests(unittest.TestCase):
    def test_non_windows_prints_error_and_returns_one(self):
        from whisper_dictate import vp_events
        with _capture_stdout() as out:
            with patch.object(vp_events.os, "name", "posix"):
                code = vp_events.print_windows()
        self.assertEqual(code, 1)
        payload = json.loads(out.getvalue())
        self.assertIn("error", payload)
        self.assertIn("Windows", payload["error"])

    def test_windows_returns_json_array_and_zero(self):
        from whisper_dictate import vp_events, vp_windows

        fake_windows = [
            {"title": "Notepad", "process": "notepad.exe"},
            {"title": "Chrome", "process": "chrome.exe"},
        ]

        with _capture_stdout() as out:
            with patch.object(vp_events.os, "name", "nt"):
                with patch.object(vp_windows, "list_visible_windows",
                                  return_value=fake_windows):
                    code = vp_events.print_windows()

        self.assertEqual(code, 0)
        payload = json.loads(out.getvalue())
        self.assertIsInstance(payload, list)
        self.assertEqual(len(payload), 2)
        self.assertEqual(payload[0]["title"], "Notepad")
        self.assertEqual(payload[1]["process"], "chrome.exe")

    def test_enumeration_exception_prints_error_and_returns_one(self):
        from whisper_dictate import vp_events, vp_windows

        with _capture_stdout() as out:
            with patch.object(vp_events.os, "name", "nt"):
                with patch.object(vp_windows, "list_visible_windows",
                                  side_effect=RuntimeError("access denied")):
                    code = vp_events.print_windows()

        self.assertEqual(code, 1)
        payload = json.loads(out.getvalue())
        self.assertIn("error", payload)
        self.assertIn("access denied", payload["error"])


# ---------------------------------------------------------------------------
# Flag wiring tests
# ---------------------------------------------------------------------------

class ListWindowsFlagTests(unittest.TestCase):
    def test_parser_exposes_list_windows_flag(self):
        from whisper_dictate.vp_cli import build_arg_parser
        args = build_arg_parser().parse_args(["--list-windows"])
        self.assertTrue(args.list_windows)

    def test_flag_defaults_off(self):
        from whisper_dictate.vp_cli import build_arg_parser
        args = build_arg_parser().parse_args([])
        self.assertFalse(args.list_windows)


# ---------------------------------------------------------------------------
# Self-injection guard constant tests
# ---------------------------------------------------------------------------

class SelfWindowFilterTests(unittest.TestCase):
    def test_own_title_plain(self):
        from whisper_dictate.vp_windows import _is_self_window
        self.assertTrue(_is_self_window("whisper-dictate", None))

    def test_own_title_with_version(self):
        from whisper_dictate.vp_windows import _is_self_window
        self.assertTrue(_is_self_window("whisper-dictate 1.8.6", None))

    def test_own_process_name(self):
        from whisper_dictate.vp_windows import _is_self_window
        self.assertTrue(_is_self_window("Some Title", "whisper-dictate.exe"))

    def test_unrelated_window_not_filtered(self):
        from whisper_dictate.vp_windows import _is_self_window
        self.assertFalse(_is_self_window("Notepad", "notepad.exe"))

    def test_partial_title_match_not_filtered(self):
        from whisper_dictate.vp_windows import _is_self_window
        # "whisper-dictate" must be a FULL match, not just a substring
        self.assertFalse(_is_self_window("My whisper-dictate notes", "notepad.exe"))


if __name__ == "__main__":
    unittest.main()
