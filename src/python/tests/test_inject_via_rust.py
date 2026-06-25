"""Phase 2.1 Rust-injector shell-out tests (`vp_inject_rust`).

Covers the small Python shim that wraps the new
``whisper-dictate inject`` subcommand:

* ``rust_injection_backend_enabled`` honours the ``VOICEPI_INJECTION_BACKEND``
  env var and is case-insensitive.
* ``inject_via_rust`` is short-circuited when ``VOICEPI_RUST_INJECTOR`` is
  unset (so the worker silently falls back to the Python path).
* On a successful invocation the JSON envelope sent to the binary carries
  every captured field (text + target + xkb_layout + method/shortcut).
* Non-zero exit, invalid JSON, and ``ok=False`` responses all surface as
  ``False`` so the caller falls back.

The mixin-level ``_inject_via_rust_backend`` dispatch is covered by
``test_injection_dispatch.py``'s existing fakes once the shim is wired up;
this file exercises the pure module helpers without spawning subprocesses.
"""
from __future__ import annotations

import json
import os
import subprocess
from contextlib import contextmanager
from unittest import TestCase
from unittest.mock import patch

from helpers import sys  # noqa: F401 — adds src/python to sys.path

from whisper_dictate import vp_inject_rust


@contextmanager
def _env(**kwargs):
    """Temporarily set ``os.environ`` entries (``None`` removes them)."""
    saved = {k: os.environ.get(k) for k in kwargs}
    try:
        for k, v in kwargs.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        yield
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class _FakeCompleted:
    def __init__(self, returncode: int, stdout: bytes = b"", stderr: bytes = b""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class RustBackendToggleTests(TestCase):
    def test_disabled_when_env_unset(self):
        with _env(VOICEPI_INJECTION_BACKEND=None):
            self.assertFalse(vp_inject_rust.rust_injection_backend_enabled())

    def test_enabled_when_set_to_rust(self):
        with _env(VOICEPI_INJECTION_BACKEND="rust"):
            self.assertTrue(vp_inject_rust.rust_injection_backend_enabled())

    def test_case_insensitive_and_trimmed(self):
        with _env(VOICEPI_INJECTION_BACKEND="  RUST  "):
            self.assertTrue(vp_inject_rust.rust_injection_backend_enabled())

    def test_other_values_disabled(self):
        for value in ("python", "auto", "1", "true", "yes", ""):
            with _env(VOICEPI_INJECTION_BACKEND=value):
                self.assertFalse(
                    vp_inject_rust.rust_injection_backend_enabled(),
                    f"unexpectedly enabled for {value!r}",
                )


class InjectViaRustTests(TestCase):
    def test_short_circuits_when_binary_unset(self):
        # No env override -> no binary -> immediate False, no subprocess call.
        with _env(VOICEPI_RUST_INJECTOR=None), \
                patch.object(subprocess, "run") as mock_run:
            self.assertFalse(vp_inject_rust.inject_via_rust("hi"))
            mock_run.assert_not_called()

    def test_success_returns_true_and_envelope_carries_all_fields(self):
        captured: dict = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["input"] = kwargs.get("input")
            captured["timeout"] = kwargs.get("timeout")
            return _FakeCompleted(
                returncode=0,
                stdout=json.dumps({"ok": True, "method": "paste:ctrl_v"}).encode(),
            )

        with _env(VOICEPI_RUST_INJECTOR="/bin/whisper-dictate"), \
                patch.object(subprocess, "run", side_effect=fake_run):
            ok = vp_inject_rust.inject_via_rust(
                "Hello world",
                mode="paste",
                shortcut="ctrl_v",
                target_title="Notepad",
                target_process="notepad.exe",
                xkb_layout="dk",
            )

        self.assertTrue(ok)
        self.assertEqual(captured["cmd"], ["/bin/whisper-dictate", "inject"])
        envelope = json.loads(captured["input"].decode())
        self.assertEqual(envelope["action"], "inject")
        self.assertEqual(envelope["text"], "Hello world")
        self.assertEqual(envelope["method"], {"mode": "paste", "shortcut": "ctrl_v"})
        self.assertEqual(envelope["target_title"], "Notepad")
        self.assertEqual(envelope["target_process"], "notepad.exe")
        self.assertEqual(envelope["xkb_layout"], "dk")

    def test_helper_arg_overrides_env(self):
        captured: dict = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            return _FakeCompleted(returncode=0, stdout=b'{"ok":true}')

        with _env(VOICEPI_RUST_INJECTOR=None), \
                patch.object(subprocess, "run", side_effect=fake_run):
            ok = vp_inject_rust.inject_via_rust("hi", helper="/opt/inject")
        self.assertTrue(ok)
        self.assertEqual(captured["cmd"], ["/opt/inject", "inject"])

    def test_nonzero_exit_returns_false(self):
        def fake_run(*_args, **_kwargs):
            return _FakeCompleted(returncode=7, stderr=b"crashed")

        with _env(VOICEPI_RUST_INJECTOR="/bin/whisper-dictate"), \
                patch.object(subprocess, "run", side_effect=fake_run):
            self.assertFalse(vp_inject_rust.inject_via_rust("hi"))

    def test_invalid_json_returns_false(self):
        def fake_run(*_args, **_kwargs):
            return _FakeCompleted(returncode=0, stdout=b"not json")

        with _env(VOICEPI_RUST_INJECTOR="/bin/whisper-dictate"), \
                patch.object(subprocess, "run", side_effect=fake_run):
            self.assertFalse(vp_inject_rust.inject_via_rust("hi"))

    def test_ok_false_response_returns_false(self):
        def fake_run(*_args, **_kwargs):
            return _FakeCompleted(
                returncode=0,
                stdout=b'{"ok":false,"error":"no helper"}',
            )

        with _env(VOICEPI_RUST_INJECTOR="/bin/whisper-dictate"), \
                patch.object(subprocess, "run", side_effect=fake_run):
            self.assertFalse(vp_inject_rust.inject_via_rust("hi"))

    def test_subprocess_exception_returns_false(self):
        def fake_run(*_args, **_kwargs):
            raise FileNotFoundError("missing")

        with _env(VOICEPI_RUST_INJECTOR="/bin/whisper-dictate"), \
                patch.object(subprocess, "run", side_effect=fake_run):
            self.assertFalse(vp_inject_rust.inject_via_rust("hi"))


class ResolveRustInjectModeTests(TestCase):
    """Codex P2 #1 — `auto` strategy must survive the Rust handoff."""

    @staticmethod
    def _resolve(user_mode, text, on_wayland, **overrides):
        defaults = dict(
            wayland_text_prefers_paste=lambda _t: False,
            text_prefers_paste=lambda _t: False,
            target_prefers_paste=lambda: False,
            ptt_is_bare_modifier=lambda: False,
        )
        defaults.update(overrides)
        return vp_inject_rust.resolve_rust_inject_mode(
            user_mode, text, on_wayland=on_wayland, **defaults
        )

    def test_explicit_paste_passes_through(self):
        self.assertEqual(self._resolve("paste", "hi", on_wayland=False), "paste")

    def test_explicit_type_passes_through(self):
        self.assertEqual(self._resolve("type", "hi", on_wayland=False), "type")

    def test_auto_types_plain_ascii_on_x11_macos(self):
        # On non-Windows with no special signals, auto should type.
        with patch("whisper_dictate.vp_inject_rust.os.name", "posix"):
            self.assertEqual(self._resolve("auto", "hi", on_wayland=False), "type")

    def test_auto_always_pastes_on_windows(self):
        # Windows: auto always pastes (pynput's per-char type drops).
        with patch("whisper_dictate.vp_inject_rust.os.name", "nt"):
            self.assertEqual(self._resolve("auto", "hi", on_wayland=False), "paste")

    def test_auto_pastes_when_text_prefers_paste(self):
        with patch("whisper_dictate.vp_inject_rust.os.name", "posix"):
            self.assertEqual(
                self._resolve(
                    "auto", "på", on_wayland=False,
                    text_prefers_paste=lambda _t: True,
                ),
                "paste",
            )

    def test_auto_pastes_when_target_prefers_paste(self):
        with patch("whisper_dictate.vp_inject_rust.os.name", "posix"):
            self.assertEqual(
                self._resolve(
                    "auto", "hi", on_wayland=False,
                    target_prefers_paste=lambda: True,
                ),
                "paste",
            )

    def test_auto_pastes_when_ptt_is_bare_modifier(self):
        with patch("whisper_dictate.vp_inject_rust.os.name", "posix"):
            self.assertEqual(
                self._resolve(
                    "auto", "hi", on_wayland=False,
                    ptt_is_bare_modifier=lambda: True,
                ),
                "paste",
            )

    def test_auto_wayland_types_plain_ascii(self):
        # Wayland branch: plain ASCII with no PTT-modifier signal -> type.
        self.assertEqual(self._resolve("auto", "hi", on_wayland=True), "type")

    def test_auto_wayland_pastes_when_text_prefers_paste(self):
        self.assertEqual(
            self._resolve(
                "auto", "på", on_wayland=True,
                wayland_text_prefers_paste=lambda _t: True,
            ),
            "paste",
        )

    def test_auto_wayland_pastes_on_bare_modifier_ptt(self):
        self.assertEqual(
            self._resolve(
                "auto", "hi", on_wayland=True,
                ptt_is_bare_modifier=lambda: True,
            ),
            "paste",
        )


class PopulateClipboardForRustPasteTests(TestCase):
    """Codex P1 #1 — Rust paste backend needs the clipboard pre-populated."""

    def setUp(self):
        # Fake pyperclip so `import pyperclip` inside the function resolves
        # to a controllable double — no native dep required in CI.
        import sys as _sys
        import types as _types

        self.pyperclip = _types.ModuleType("pyperclip")
        self.pyperclip.copied = []
        self.pyperclip.stored = "prev-clip"

        def _copy(text):
            self.pyperclip.copied.append(text)

        def _paste():
            return self.pyperclip.stored

        self.pyperclip.copy = _copy
        self.pyperclip.paste = _paste
        self._restore_real = _sys.modules.get("pyperclip")
        _sys.modules["pyperclip"] = self.pyperclip

    def tearDown(self):
        import sys as _sys
        if self._restore_real is None:
            _sys.modules.pop("pyperclip", None)
        else:
            _sys.modules["pyperclip"] = self._restore_real

    def test_copies_text_to_clipboard(self):
        ok = vp_inject_rust.populate_clipboard_for_rust_paste(
            "hello", restore_enabled=False,
        )
        self.assertTrue(ok)
        self.assertEqual(self.pyperclip.copied, ["hello"])

    def test_returns_false_when_pyperclip_missing(self):
        # Replace with a module that raises on import — easier: pop and
        # have the import fail via a sentinel raising attribute access.
        import sys as _sys
        _sys.modules.pop("pyperclip", None)

        class _Boom:
            def __getattr__(self, _name):
                raise ImportError("pyperclip not installed")

        # Setting to a normal module that raises on copy() exercises the
        # outer try/except. To exercise the import-error branch we need
        # the import itself to fail — simulate by deleting and blocking.
        import importlib

        class _Finder:
            def find_spec(self, name, _path, _target=None):
                if name == "pyperclip":
                    raise ImportError("pyperclip not installed")
                return None

        finder = _Finder()
        _sys.meta_path.insert(0, finder)
        try:
            importlib.invalidate_caches()
            ok = vp_inject_rust.populate_clipboard_for_rust_paste("hi")
            self.assertFalse(ok)
        finally:
            _sys.meta_path.remove(finder)

    def test_schedules_restore_when_enabled(self):
        calls: list = []

        def fake_restore(pyperclip_mod, injected, previous):
            calls.append((injected, previous))

        ok = vp_inject_rust.populate_clipboard_for_rust_paste(
            "hello",
            restore_enabled=True,
            restore_after_delay=fake_restore,
        )
        self.assertTrue(ok)
        # Thread is daemon; give it a moment to run.
        import time
        for _ in range(50):
            if calls:
                break
            time.sleep(0.01)
        self.assertEqual(calls, [("hello", "prev-clip")])
