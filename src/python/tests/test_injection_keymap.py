import subprocess
import tempfile
from pathlib import Path

from helpers import (
    _env,
    _capture_stdout,
    contextmanager,
    load_voice_pi,
    os,
    patch,
    sys,
    types,
    unittest,
)

class InjectStrategyTests(unittest.TestCase):
    def setUp(self):
        for n in ("vp_inject",):
            sys.modules.pop(n, None)
        from whisper_dictate import vp_inject
        self.inject = vp_inject

    def _dummy(self, title=None, process=None):
        return types.SimpleNamespace(
            _inject_target_title=title,
            _inject_target_process=process,
        )

    def _injector(
            self,
            mode="auto",
            paste_ok=True,
            wayland_ok=True,
            title=None,
            process=None):
        mixin = self.inject.InjectMixin

        class Dummy(mixin):
            def __init__(self):
                self.mode = mode
                self.typed = []
                self._kb = types.SimpleNamespace(type=self.typed.append)
                self._inject_target_xwin = None
                self._inject_target_title = title
                self._inject_target_process = process
                self.pasted = []
                self.typed_wayland = []
                self.ydotool = []
                self.rust_inject = []

            def _restore_target_focus(self):
                return False

            def _paste(self, text):
                self.pasted.append(text)
                return paste_ok

            def _wayland_type(self, text):
                self.typed_wayland.append(text)
                return wayland_ok

            def _try_ydotool(self, *args):
                self.ydotool.append(args)
                return True

            def _try_rust_inject(self, mode, text=""):
                self.rust_inject.append((mode, text))
                return False

        return Dummy()

    def test_windows_terminal_targets_prefer_paste(self):
        target = self._dummy("Administrator: Windows PowerShell", "WindowsTerminal.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertTrue(
                self.inject.InjectMixin._target_prefers_paste(target))

    def test_regular_windows_targets_still_type(self):
        target = self._dummy("Untitled - Notepad", "notepad.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertFalse(
                self.inject.InjectMixin._target_prefers_paste(target))

    def test_windows_layout_sensitive_text_prefers_paste(self):
        target = self._dummy("Untitled - Notepad", "notepad.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertTrue(
                self.inject.InjectMixin._text_prefers_paste(target, "I'm testing"))
            self.assertTrue(
                self.inject.InjectMixin._text_prefers_paste(target, 'say "hello"'))
            self.assertFalse(
                self.inject.InjectMixin._text_prefers_paste(target, "plain ascii"))

    def test_windows_auto_pastes_layout_sensitive_text(self):
        with open("src/python/whisper_dictate/vp_inject.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("_WINDOWS_LAYOUT_SENSITIVE_CHARS", script)
        self.assertIn("self._text_prefers_paste(text)", script)

    def test_non_windows_targets_still_type(self):
        target = self._dummy("Windows Terminal", "WindowsTerminal.exe")

        with patch.object(self.inject.os, "name", "posix"):
            self.assertFalse(
                self.inject.InjectMixin._target_prefers_paste(target))

    def test_inject_skips_self_target_but_keeps_final_preview_log(self):
        target = self._injector(
            title="whisper-dictate 1.0.0",
            process="whisper-dictate.exe",
        )

        with patch.object(self.inject.os, "name", "nt"), _capture_stdout() as stdout:
            target._inject("Og hvad kan vi så se her?")

        self.assertEqual(target._last_inject_strategy, "skipped-self")
        self.assertEqual(target.typed, [])
        self.assertEqual(target.pasted, [])
        self.assertIn('[inject] → "Og hvad kan vi så se her?"', stdout.getvalue())
        self.assertIn("[inject] skipped self-target", stdout.getvalue())

    def test_terminal_title_containing_project_name_is_not_self_target(self):
        target = self._dummy("whisper-dictate - Terminal", "WindowsTerminal.exe")

        self.assertFalse(self.inject.InjectMixin._target_is_self(target))

    def test_self_injection_title_regex_matches_versions_without_redos(self):
        # ReDoS-safe rewrite (Sonar S5852): no ambiguous bounded-repeat-then-.*.
        # Match our own window title — "whisper-dictate" + optional space + a
        # digit-led version + anything, so it also covers the git-describe /
        # -dirty suffixes runtime::version() can return. Reject look-alikes.
        rx = self.inject._SELF_INJECTION_TITLE_RE
        for title in (
            "whisper-dictate",
            "whisper-dictate 1",
            "whisper-dictate 1.0.0",
            "whisper-dictate 1.7.0.0",
            "whisper-dictate 1.7.0 settings",
            "whisper-dictate 1.7.0-5-gabc1234",      # git describe
            "whisper-dictate 1.7.0-dirty",
            "whisper-dictate 1.7.0-" + "a" * 5000,   # long suffix resolves promptly
        ):
            self.assertTrue(rx.fullmatch(title), title)
        for title in (
            "whisper-dictate - terminal",  # terminal window, not us
            "whisper-dictatex",
            "my whisper-dictate",
            "whisper-dictate foo",          # trailing must start with a version digit
        ):
            self.assertFalse(rx.fullmatch(title), title)

    def test_wayland_auto_pastes_non_ascii_text(self):
        target = self._injector(mode="auto")

        with _env(WAYLAND_DISPLAY="wayland-0"):
            target._inject("Æbler, ører og øjne")

        self.assertEqual(target._last_inject_strategy, "paste")
        self.assertEqual(target.pasted, ["Æbler, ører og øjne"])
        self.assertEqual(target.typed_wayland, [])

    def test_wayland_auto_uses_ydotool_for_ascii_text(self):
        target = self._injector(mode="auto")

        with _env(WAYLAND_DISPLAY="wayland-0"):
            target._inject("plain ascii")

        self.assertEqual(target._last_inject_strategy, "ydotool")
        self.assertEqual(target.pasted, [])
        self.assertEqual(target.typed_wayland, ["plain ascii"])

    def test_wayland_auto_falls_back_to_ydotool_when_paste_fails(self):
        target = self._injector(mode="auto", paste_ok=False)

        with _env(WAYLAND_DISPLAY="wayland-0"):
            target._inject("øjne")

        self.assertEqual(target._last_inject_strategy, "ydotool")
        self.assertEqual(target.pasted, ["øjne"])
        self.assertEqual(target.typed_wayland, ["øjne"])

    def test_wayland_explicit_type_keeps_direct_injection_for_non_ascii(self):
        target = self._injector(mode="type")

        with _env(WAYLAND_DISPLAY="wayland-0"):
            target._inject("øjne")

        self.assertEqual(target._last_inject_strategy, "ydotool")
        self.assertEqual(target.pasted, [])
        self.assertEqual(target.typed_wayland, ["øjne"])

    def test_wayland_unknown_target_uses_terminal_paste_shortcut(self):
        target = self._injector()

        self.assertTrue(target._wayland_paste_shortcut())

        self.assertEqual(len(target.ydotool), 2)
        self.assertEqual(target.ydotool[0][0], "key")
        self.assertIn("29:0", target.ydotool[0])
        self.assertIn("42:0", target.ydotool[0])
        self.assertEqual(
            target.ydotool[1],
            ("key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"),
        )

    def test_wayland_terminal_target_uses_ctrl_shift_v(self):
        target = self._injector(title="whisper-dictate - Terminal")

        self.assertTrue(target._wayland_paste_shortcut())

        self.assertEqual(
            target.ydotool[1],
            ("key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"),
        )

    def test_wayland_known_non_terminal_target_uses_ctrl_v(self):
        target = self._injector(title="Text Editor", process="gnome-text-editor")

        self.assertTrue(target._wayland_paste_shortcut())

        self.assertEqual(target.ydotool[1], ("key", "29:1", "47:1", "47:0", "29:0"))

    def test_wayland_type_uses_rust_injector_before_python_ydotool(self):
        target = self._injector()
        target._xkb_layout = "dk"
        target._try_rust_inject = lambda mode, text="": True

        self.assertTrue(target._wayland_type("høre"))

        self.assertEqual(target.ydotool, [])

    def test_wayland_paste_uses_rust_injector_before_python_ydotool(self):
        target = self._injector()
        target._try_rust_inject = lambda mode, text="": True

        self.assertTrue(target._wayland_paste_shortcut())

        self.assertEqual(target.ydotool, [])


    def test_wayland_type_without_rust_injector_keeps_ascii_ydotool_fallback(self):
        target = self._injector()
        target._xkb_layout = "dk"

        self.assertTrue(self.inject.InjectMixin._wayland_type(target, "plain ascii"))

        self.assertEqual(target.ydotool, [("type", "--", "plain ascii")])

    def test_wayland_type_without_rust_injector_rejects_non_ascii_direct_typing(self):
        target = self._injector()
        target._xkb_layout = "dk"

        self.assertFalse(self.inject.InjectMixin._wayland_type(target, "høre"))

        self.assertEqual(target.ydotool, [])


class YdotoolReadinessTests(unittest.TestCase):
    def _socket_path(self) -> str:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        return str(Path(self._tmp.name) / "ydotool.sock")

    def test_ready_accepts_ydotool_debug_even_when_socket_connect_fails(self):
        socket_path = self._socket_path()
        from whisper_dictate import vp_inject

        with patch.object(vp_inject.shutil, "which", return_value="/usr/bin/ydotool"), \
             patch.object(vp_inject.subprocess, "run") as run, \
             patch.object(vp_inject, "unix_socket_connect_ready", return_value=False):
            run.return_value = subprocess.CompletedProcess(["ydotool", "debug"], 0)

            self.assertTrue(vp_inject.ydotoold_ready(socket_path, timeout=0.1))

            _, kwargs = run.call_args
            self.assertEqual(kwargs["env"]["YDOTOOL_SOCKET"], socket_path)

    def test_ready_falls_back_to_socket_connect_without_ydotool_client(self):
        socket_path = self._socket_path()
        from whisper_dictate import vp_inject

        with patch.object(vp_inject.shutil, "which", return_value=None), \
             patch.object(vp_inject, "unix_socket_connect_ready", return_value=True):
            self.assertTrue(vp_inject.ydotoold_ready(socket_path, timeout=0.1))


@contextmanager
def _env(**kwargs):
    old = {k: os.environ.get(k) for k in kwargs}
    for k, v in kwargs.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    try:
        yield
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

class DetectXkbLayoutTests(unittest.TestCase):
    """_detect_xkb_layout: prioritetsrækkefølge og fallback."""

    def setUp(self):
        self.vp = load_voice_pi()
        # Ryd env-variabler der ellers forstyrrer
        self._patches = [
            patch.dict(os.environ, {}, clear=False),
        ]
        for p in self._patches:
            p.start()
        os.environ.pop('VOICEPI_XKB_LAYOUT', None)
        os.environ.pop('XKB_DEFAULT_LAYOUT', None)

    def tearDown(self):
        for p in self._patches:
            p.stop()

    def test_voicepi_env_var_takes_priority(self):
        with _env(VOICEPI_XKB_LAYOUT='se', XKB_DEFAULT_LAYOUT='de'):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'se')

    def test_voicepi_env_lang_code_da_maps_to_dk(self):
        with _env(VOICEPI_XKB_LAYOUT='da', XKB_DEFAULT_LAYOUT='de'):
            result = self.vp._detect_xkb_layout(None)
        self.assertEqual(result, 'dk')

    def test_voicepi_env_invalid_en_does_not_block_lang_fallback(self):
        with _env(VOICEPI_XKB_LAYOUT='en'):
            with patch('builtins.open', side_effect=FileNotFoundError):
                result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'dk')

    def test_xkb_default_layout_beats_keyboard_file(self):
        with _env(XKB_DEFAULT_LAYOUT='de'):
            with patch('builtins.open', side_effect=FileNotFoundError):
                result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'de')

    def test_keyboard_file_parsed_correctly(self):
        content = 'XKBLAYOUT="dk"\nXKBVARIANT=""\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout(None)
        self.assertEqual(result, 'dk')

    def test_us_layout_in_keyboard_file_is_ignored(self):
        content = 'XKBLAYOUT="us"\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout('da')
        # Falder igennem til lang-hint: da → dk
        self.assertEqual(result, 'dk')

    def test_lang_hint_da_gives_dk(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'dk')

    def test_lang_hint_nb_gives_no(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('nb')
        self.assertEqual(result, 'no')

    def test_no_hints_returns_none(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout(None)
        self.assertIsNone(result)
