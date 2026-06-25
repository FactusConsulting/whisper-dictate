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
