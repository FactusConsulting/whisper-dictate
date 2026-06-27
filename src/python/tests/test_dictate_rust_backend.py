"""Tests for ``VOICEPI_DICTATE_BACKEND=rust`` shell-out (Wave 5 of #348).

The Rust ops dispatcher lives in ``src/rust/dictate/ops.rs`` and is reached
via ``whisper-dictate dictate-ops``; this file exercises:

* env-var gating (unset env -> Python path, set env -> shell-out)
* missing helper / non-zero exit / invalid JSON -> graceful Python fallback
* successful shell-out -> decoded into the shape Python callers expect
* the helper's ``invalid VOICEPI_STT_BACKEND=...`` rejection is surfaced
  as ``ValueError`` so ``argparse.ap.error`` prints the same line whether
  the validation ran in Rust or in Python.

The Rust binary is NOT invoked — we mock ``subprocess.run`` so the test
runs on machines without a built Rust binary (CI's Python lane).
"""
from __future__ import annotations

import json
import os
import sys
import unittest
from unittest import mock


HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from whisper_dictate import vp_dictate, vp_dictate_rust  # noqa: E402


def _fake_completed(*, returncode: int = 0, stdout: str = "", stderr: str = ""):
    """Stand-in for ``subprocess.CompletedProcess`` with the attrs we touch."""
    return mock.MagicMock(returncode=returncode, stdout=stdout, stderr=stderr)


class _EnvSnapshot(unittest.TestCase):
    """Mixin: snapshot + restore the two env vars every shell-out path uses."""

    KEYS = ("VOICEPI_DICTATE_BACKEND", "VOICEPI_RUST_INJECTOR")

    def setUp(self) -> None:  # noqa: D401 - inherited hook
        super().setUp()
        self._prev = {key: os.environ.get(key) for key in self.KEYS}

    def tearDown(self) -> None:  # noqa: D401 - inherited hook
        for key, prev in self._prev.items():
            if prev is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = prev
        super().tearDown()


class BackendEnabledTests(_EnvSnapshot):
    def test_default_is_disabled(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        self.assertFalse(vp_dictate_rust.rust_backend_enabled())

    def test_explicit_rust_enables(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        self.assertTrue(vp_dictate_rust.rust_backend_enabled())

    def test_other_values_do_not_enable(self) -> None:
        for v in ("python", "off", "", "PYTHON", "1"):
            os.environ["VOICEPI_DICTATE_BACKEND"] = v
            self.assertFalse(
                vp_dictate_rust.rust_backend_enabled(),
                f"unexpected enable for {v!r}",
            )

    def test_case_and_whitespace_insensitive(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "  RUST  "
        self.assertTrue(vp_dictate_rust.rust_backend_enabled())


class CallOpTests(_EnvSnapshot):
    def test_unset_env_returns_none_no_subprocess(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            self.assertIsNone(vp_dictate_rust.call_op("should_skip", {}))
        run.assert_not_called()

    def test_helper_missing_returns_none(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            self.assertIsNone(vp_dictate_rust.call_op("should_skip", {}))
        run.assert_not_called()

    def test_helper_nonzero_exit_falls_back(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=2, stderr="boom"),
        ):
            self.assertIsNone(vp_dictate_rust.call_op("should_skip", {}))

    def test_helper_invalid_json_falls_back(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout="not valid {json"),
        ):
            self.assertIsNone(vp_dictate_rust.call_op("should_skip", {}))

    def test_subprocess_exception_returns_none(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run", side_effect=OSError("nope"),
        ):
            self.assertIsNone(vp_dictate_rust.call_op("should_skip", {}))

    def test_success_returns_parsed_payload(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        payload = {"decision": "keep", "reason": None, "hint": None}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(payload)),
        ):
            body = vp_dictate_rust.call_op("should_skip", {"samples": 1})
        self.assertEqual(body, payload)

    def test_envelope_carries_op_and_params(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        seen = {}

        def _run(cmd, *, input, **_kw):
            seen["cmd"] = cmd
            seen["envelope"] = json.loads(input)
            return _fake_completed(stdout="{}")

        with mock.patch.object(vp_dictate_rust.subprocess, "run", side_effect=_run):
            vp_dictate_rust.call_op("changed_restart_keys", {"before": {}, "after": {}})

        self.assertEqual(seen["cmd"], ["/fake/whisper-dictate", "dictate-ops"])
        self.assertEqual(seen["envelope"]["op"], "changed_restart_keys")
        self.assertEqual(seen["envelope"]["params"], {"before": {}, "after": {}})


class ValidateBackendTests(_EnvSnapshot):
    def test_disabled_returns_none(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            self.assertIsNone(vp_dictate_rust.rust_validate_backend("whisper"))
        run.assert_not_called()

    def test_success_decodes_canonical_and_label(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        body = {"backend": "whisper", "label": "Whisper"}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(body)),
        ):
            result = vp_dictate_rust.rust_validate_backend("faster-whisper")
        self.assertEqual(result, ("whisper", "Whisper"))

    def test_helper_invalid_backend_stderr_raises_valueerror(self) -> None:
        # The Rust helper exits non-zero with an `invalid VOICEPI_STT_BACKEND=…`
        # line on stderr; this must SURFACE as ValueError so the runtime path
        # prints the same `ap.error(...)` line the Python validation does.
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        stderr = (
            "error: invalid VOICEPI_STT_BACKEND=\"groq\"; "
            "expected one of whisper, openai"
        )
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=1, stderr=stderr),
        ):
            with self.assertRaises(ValueError) as cm:
                vp_dictate_rust.rust_validate_backend("groq")
        self.assertIn("invalid VOICEPI_STT_BACKEND", str(cm.exception))
        self.assertIn("groq", str(cm.exception))

    def test_helper_error_prefix_stripped_before_raising(self) -> None:
        # Regression for PR #359: the Rust launcher emits errors via
        # `eprintln!("error: {err}")`, so stderr starts with `error: `.
        # The ValueError message MUST NOT keep that prefix, because the
        # caller does `ap.error(str(e))` and argparse adds its own
        # `error: ` prefix, which would yield `error: error: invalid ...`.
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        stderr = (
            "error: invalid VOICEPI_STT_BACKEND=\"groq\"; "
            "expected one of whisper, openai"
        )
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=1, stderr=stderr),
        ):
            with self.assertRaises(ValueError) as cm:
                vp_dictate_rust.rust_validate_backend("groq")
        msg = str(cm.exception)
        self.assertFalse(
            msg.startswith("error:"),
            f"ValueError message must not retain Rust launcher prefix: {msg!r}",
        )
        self.assertTrue(msg.startswith("invalid VOICEPI_STT_BACKEND"), msg)

    def test_other_failure_returns_none_for_fallback(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=1, stderr="unrelated boom"),
        ):
            self.assertIsNone(vp_dictate_rust.rust_validate_backend("whisper"))


class ChangedRestartKeysTests(_EnvSnapshot):
    def test_disabled_returns_none(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            self.assertIsNone(
                vp_dictate_rust.rust_changed_restart_keys({}, {}))
        run.assert_not_called()

    def test_decodes_changed_list(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        body = {"changed": ["device", "model"]}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(body)),
        ):
            self.assertEqual(
                vp_dictate_rust.rust_changed_restart_keys(
                    {"model": "tiny", "device": "cpu"},
                    {"model": "large-v3-turbo", "device": "cuda"},
                ),
                ["device", "model"],
            )


# ---------------------------------------------------------------- vp_dictate wiring

class ChangedRestartKeysOptRustTests(_EnvSnapshot):
    """The Python wrapper falls back to the in-process diff when the gate
    is off / the helper is unavailable, and uses the Rust answer when it
    succeeds. Behaviour must stay byte-identical on the default path."""

    def test_default_uses_python_diff(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            changed = vp_dictate._changed_restart_keys_opt_rust(
                {"model": "tiny"}, {"model": "large-v3-turbo"})
        run.assert_not_called()
        self.assertEqual(changed, ["model"])

    def test_default_diff_is_sorted_alphabetically(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        changed = vp_dictate._changed_restart_keys_opt_rust(
            {"stt_backend": "whisper", "device": "cpu"},
            {"stt_backend": "openai", "device": "cuda"},
        )
        self.assertEqual(changed, ["device", "stt_backend"])

    def test_unrelated_key_change_is_ignored(self) -> None:
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        changed = vp_dictate._changed_restart_keys_opt_rust(
            {"min_record_seconds": "0.5"}, {"min_record_seconds": "0.9"})
        self.assertEqual(changed, [])

    def test_rust_answer_overrides_python_diff_when_enabled(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        body = {"changed": ["device"]}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(body)),
        ):
            changed = vp_dictate._changed_restart_keys_opt_rust(
                {"model": "tiny", "device": "cpu"},
                {"model": "tiny", "device": "cuda"})
        self.assertEqual(changed, ["device"])

    def test_rust_failure_falls_back_to_python(self) -> None:
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=2, stderr="oops"),
        ):
            changed = vp_dictate._changed_restart_keys_opt_rust(
                {"model": "tiny"}, {"model": "large-v3-turbo"})
        # Python fallback gives the same answer the Rust helper would have.
        self.assertEqual(changed, ["model"])


class ValidateBackendOptRustTests(_EnvSnapshot):
    """``runtime._validate_backend_opt_rust`` wires the dictate-ops helper
    into the legacy ``VALID_STT_BACKENDS`` membership check. With the gate
    off the in-Python check runs unchanged; with the gate on the Rust
    decision wins (and its ``ValueError`` is re-raised verbatim)."""

    def test_default_uses_python_validation_passes(self) -> None:
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with mock.patch.object(vp_dictate_rust.subprocess, "run") as run:
            self.assertEqual(runtime._validate_backend_opt_rust("whisper"), "whisper")
        run.assert_not_called()

    def test_default_python_rejects_unknown(self) -> None:
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        with self.assertRaises(ValueError) as cm:
            runtime._validate_backend_opt_rust("groq")
        self.assertIn("invalid VOICEPI_STT_BACKEND", str(cm.exception))
        self.assertIn("groq", str(cm.exception))

    def test_rust_success_returns_canonical(self) -> None:
        from whisper_dictate import runtime
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        body = {"backend": "whisper", "label": "Whisper"}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(body)),
        ):
            self.assertEqual(
                runtime._validate_backend_opt_rust("faster-whisper"), "whisper")

    def test_rust_rejection_raises_valueerror(self) -> None:
        from whisper_dictate import runtime
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        stderr = (
            "error: invalid VOICEPI_STT_BACKEND=\"groq\"; "
            "expected one of whisper, openai"
        )
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=1, stderr=stderr),
        ):
            with self.assertRaises(ValueError) as cm:
                runtime._validate_backend_opt_rust("groq")
        self.assertIn("groq", str(cm.exception))

    def test_lightweight_validation_does_not_import_vp_transcribe(self) -> None:
        # Regression for PR #359: this helper is reachable from the
        # lightweight `--help` and unit-test paths that run BEFORE
        # `_load_runtime_modules()` materialises the ML stack. The
        # fallback Python membership check must therefore NOT import
        # `vp_transcribe` (which imports numpy + faster_whisper at
        # module-import time). We assert by:
        #   1. clearing any cached `runtime.VALID_STT_BACKENDS` so the
        #      lazy `__getattr__` would have to re-import,
        #   2. stubbing `vp_transcribe` out of `sys.modules` so a re-import
        #      would crash with ImportError.
        # If the helper still returns a correct answer, it must have used
        # the local copy and skipped the import.
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        runtime_globals = vars(runtime)
        prior_valid = runtime_globals.pop("VALID_STT_BACKENDS", None)
        try:
            with mock.patch.dict(sys.modules, {"whisper_dictate.vp_transcribe": None}):
                # `whisper` is valid — must not raise, must not import vp_transcribe.
                self.assertEqual(runtime._validate_backend_opt_rust("whisper"), "whisper")
                # `groq` is rejected — error wording must still be byte-identical.
                with self.assertRaises(ValueError) as cm:
                    runtime._validate_backend_opt_rust("groq")
                self.assertIn("invalid VOICEPI_STT_BACKEND", str(cm.exception))
                self.assertIn("groq", str(cm.exception))
        finally:
            if prior_valid is not None:
                runtime_globals["VALID_STT_BACKENDS"] = prior_valid

    def test_local_valid_backends_match_canonical_definition(self) -> None:
        # The local mirror must stay in sync with vp_transcribe's authoritative
        # tuple — otherwise the lightweight path could diverge from the post-
        # `_load_runtime_modules()` path. This is the sync-guard test referenced
        # by the comment in `_validate_backend_opt_rust`.
        from whisper_dictate import runtime
        from whisper_dictate.vp_transcribe import VALID_STT_BACKENDS
        self.assertEqual(
            tuple(runtime._VALID_STT_BACKENDS_LOCAL),
            tuple(VALID_STT_BACKENDS),
        )


class BackendLabelOptRustTests(_EnvSnapshot):
    """The label-resolution helper mirrors ``_resolve_model_name``'s
    backend→label branch. Default path stays in Python; opt-in path
    sources the label from the dictate-ops helper."""

    def test_default_whisper_label(self) -> None:
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        self.assertEqual(runtime._backend_label_opt_rust("whisper"), "Whisper")

    def test_default_legacy_parakeet_label_falls_back_to_whisper(self) -> None:
        # Wave 8 of #348 dropped the NeMo/Parakeet backend. The label resolver
        # now defaults the legacy value to the Whisper label so a stale env
        # var doesn't surface "NVIDIA Parakeet" anywhere in the UI/log.
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        self.assertEqual(
            runtime._backend_label_opt_rust("parakeet"), "Whisper")

    def test_default_openai_label(self) -> None:
        from whisper_dictate import runtime
        os.environ.pop("VOICEPI_DICTATE_BACKEND", None)
        self.assertEqual(
            runtime._backend_label_opt_rust("openai"), "External API")

    def test_rust_overrides_label_when_enabled(self) -> None:
        # The Rust helper's response is trusted verbatim; the only label it
        # emits now (post Wave 8 of #348) is one of "Whisper" / "External API".
        from whisper_dictate import runtime
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        body = {"backend": "openai", "label": "External API"}
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(stdout=json.dumps(body)),
        ):
            self.assertEqual(
                runtime._backend_label_opt_rust("openai"), "External API")

    def test_rust_failure_falls_back_to_python_label(self) -> None:
        # When the Rust helper is unavailable the in-Python fallback runs.
        # Legacy ``"parakeet"`` now resolves to the Whisper label (Wave 8 of
        # #348) so a stale env var no longer surfaces "NVIDIA Parakeet".
        from whisper_dictate import runtime
        os.environ["VOICEPI_DICTATE_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictate_rust.subprocess, "run",
            return_value=_fake_completed(returncode=2, stderr="oops"),
        ):
            self.assertEqual(
                runtime._backend_label_opt_rust("parakeet"), "Whisper")


if __name__ == "__main__":
    unittest.main()
