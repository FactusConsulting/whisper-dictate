"""Tests for the ``VOICEPI_POSTPROCESS_BACKEND=rust`` shell-out in vp_postprocess
and the ``VOICEPI_EXTERNAL_API_BACKEND=rust`` shell-out in vp_external_api.

Wave 4-B of the Python-removal roadmap (#348). The Rust ports live in
``src/rust/postprocess/`` and ``src/rust/cloud_api/chat.rs`` and are reached
via ``whisper-dictate postprocess`` / ``whisper-dictate external-api``; this
file exercises the gating + fallback behaviour without relying on a real
binary (we patch ``subprocess.run`` and assert the envelope shape).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import unittest
from unittest import mock


HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))


_ENV_KEYS = (
    "VOICEPI_POSTPROCESS_BACKEND",
    "VOICEPI_EXTERNAL_API_BACKEND",
    "VOICEPI_RUST_INJECTOR",
    "VOICEPI_POST_PROCESSOR",
    "VOICEPI_POST_API_KEY",
    "OPENAI_API_KEY",
    "GROQ_API_KEY",
    "VOICEPI_LOCAL_ONLY",
)


def _env_clear():
    """Clear the env vars this test class manipulates.

    Note: we intentionally do NOT pop modules from sys.modules. Popping the
    fresh-loaded ``whisper_dictate.vp_postprocess`` mid-suite produces a
    second module instance whose functions still hold ``__globals__`` to the
    original dict, which means ``mock.patch`` on the package path patches a
    different copy than the one running — the failure mode that triggered
    the test_cleanup_call_gets_length_scaled_timeout leak this comment
    documents. Env-var isolation is enough.
    """
    for key in _ENV_KEYS:
        os.environ.pop(key, None)


class PostprocessRustBackendTests(unittest.TestCase):
    def setUp(self) -> None:
        self._snapshot = dict(os.environ)
        _env_clear()

    def tearDown(self) -> None:
        # Only restore the keys we touched, so we don't introduce or remove
        # unrelated env vars (which would defeat the next test's setUp).
        for key in _ENV_KEYS:
            previous = self._snapshot.get(key)
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous

    # --- env-var gating ------------------------------------------------------

    def test_unset_env_takes_python_path(self):
        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        # No env var: helper must never be consulted.
        with mock.patch("whisper_dictate.vp_postprocess.subprocess.run") as run:
            result = vp_postprocess.postprocess_text("fallback text", settings)

        run.assert_not_called()
        self.assertTrue(result.fallback)

    def test_backend_set_but_no_helper_falls_through(self):
        os.environ["VOICEPI_POSTPROCESS_BACKEND"] = "rust"
        # VOICEPI_RUST_INJECTOR intentionally unset — helper_path() returns None.
        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        with mock.patch("whisper_dictate.vp_postprocess.subprocess.run") as run:
            result = vp_postprocess.postprocess_text("fallback text", settings)

        run.assert_not_called()
        self.assertTrue(result.fallback)

    # --- successful shell-out -----------------------------------------------

    def test_rust_helper_success_returns_parsed_result(self):
        os.environ["VOICEPI_POSTPROCESS_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_postprocess

        rust_payload = {
            "text": "Cleaned text.",
            "raw_text": "cleaned text",
            "changed": True,
            "provider": "openai",
            "mode": "clean",
            "model": "gpt-4o-mini",
            "latency_ms": 42,
            "fallback": False,
            "error": "",
            "redacted": False,
            "redactions": [],
        }
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout=json.dumps(rust_payload), stderr=""
        )
        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url="https://api.openai.com/v1",
            timeout_ms=4000,
            api_key="test-key",
        )

        with mock.patch(
            "whisper_dictate.vp_postprocess.subprocess.run", return_value=completed
        ) as run:
            result = vp_postprocess.postprocess_text("cleaned text", settings)

        run.assert_called_once()
        args = run.call_args.args[0]
        self.assertEqual(args[:2], ["whisper-dictate", "postprocess"])
        payload = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(payload["action"], "process")
        self.assertEqual(payload["text"], "cleaned text")
        self.assertEqual(payload["settings"]["processor"], "openai")
        self.assertEqual(payload["settings"]["model"], "gpt-4o-mini")
        self.assertEqual(result.text, "Cleaned text.")
        self.assertTrue(result.changed)
        self.assertEqual(result.latency_ms, 42)
        self.assertEqual(result.provider, "openai")

    def test_rust_helper_non_zero_exit_falls_back_to_python(self):
        os.environ["VOICEPI_POSTPROCESS_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_postprocess

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 1, stdout="", stderr="boom"
        )
        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        with mock.patch(
            "whisper_dictate.vp_postprocess.subprocess.run", return_value=completed
        ):
            result = vp_postprocess.postprocess_text("fallback text", settings)

        # Python fallback kicked in: ollama at port 1 always fails, so we get
        # the original text back with fallback=True.
        self.assertEqual(result.text, "fallback text")
        self.assertTrue(result.fallback)

    def test_rust_helper_bad_json_falls_back_to_python(self):
        os.environ["VOICEPI_POSTPROCESS_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_postprocess

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout="not json", stderr=""
        )
        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        with mock.patch(
            "whisper_dictate.vp_postprocess.subprocess.run", return_value=completed
        ):
            result = vp_postprocess.postprocess_text("fallback text", settings)

        self.assertEqual(result.text, "fallback text")
        self.assertTrue(result.fallback)

    def test_rust_helper_passes_local_only_through_envelope(self):
        os.environ["VOICEPI_POSTPROCESS_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_postprocess
        # Set AFTER import because `apply_config_to_environ()` (run at module
        # load) overlays the saved test-config's `local_only=false` over the
        # env var, so a pre-import "1" would be clobbered before the test
        # could observe it.
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"

        # The Rust helper performs its own local-only check; we just need to
        # confirm the Python wiring propagates the flag.
        rust_payload = {
            "text": "x",
            "raw_text": "x",
            "changed": False,
            "provider": "ollama",
            "mode": "clean",
            "model": "qwen2.5:3b",
            "latency_ms": 0,
            "fallback": False,
            "error": "",
            "redacted": False,
            "redactions": [],
        }
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout=json.dumps(rust_payload), stderr=""
        )
        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url="http://127.0.0.1:11434",
            timeout_ms=100,
        )
        with mock.patch(
            "whisper_dictate.vp_postprocess.subprocess.run", return_value=completed
        ) as run:
            vp_postprocess.postprocess_text("x", settings)

        payload = json.loads(run.call_args.kwargs["input"])
        self.assertTrue(payload["settings"]["local_only"])


class ExternalApiRustBackendTests(unittest.TestCase):
    def setUp(self) -> None:
        self._snapshot = dict(os.environ)
        _env_clear()

    def tearDown(self) -> None:
        # Only restore the keys we touched, so we don't introduce or remove
        # unrelated env vars (which would defeat the next test's setUp).
        for key in _ENV_KEYS:
            previous = self._snapshot.get(key)
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous

    def test_unset_env_takes_python_path(self):
        from whisper_dictate import vp_external_api

        # No env var: helper must never be consulted (we only assert run isn't
        # called for the rust gating; the urllib path would be exercised by
        # the regular external-api tests if a server were available).
        with mock.patch(
            "whisper_dictate.vp_external_api.subprocess.run"
        ) as run, mock.patch.object(
            vp_external_api,
            "_request_json",
            return_value={"choices": [{"message": {"content": "ok"}}]},
        ):
            text, _ = vp_external_api.openai_chat_completion(
                base_url="https://api.openai.com/v1",
                api_key="test-key",
                model="gpt-4o-mini",
                prompt="hello",
                timeout_ms=1000,
            )

        run.assert_not_called()
        self.assertEqual(text, "ok")

    def test_rust_helper_success_returns_text_and_latency(self):
        os.environ["VOICEPI_EXTERNAL_API_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_external_api

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"],
            0,
            stdout=json.dumps({"text": "Cleaned text.", "latency_ms": 42}),
            stderr="",
        )
        with mock.patch(
            "whisper_dictate.vp_external_api.subprocess.run", return_value=completed
        ) as run:
            text, latency = vp_external_api.openai_chat_completion(
                base_url="https://api.openai.com/v1",
                api_key="test-key",
                model="gpt-4o-mini",
                prompt="clean this",
                timeout_ms=1000,
            )

        args = run.call_args.args[0]
        self.assertEqual(args[:2], ["whisper-dictate", "external-api"])
        payload = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(payload["action"], "chat_completion")
        self.assertEqual(payload["base_url"], "https://api.openai.com/v1")
        self.assertEqual(payload["api_key"], "test-key")
        self.assertEqual(payload["model"], "gpt-4o-mini")
        self.assertEqual(text, "Cleaned text.")
        self.assertEqual(latency, 42)

    def test_rust_helper_non_zero_exit_falls_back_to_python(self):
        os.environ["VOICEPI_EXTERNAL_API_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_external_api

        rust_failure = subprocess.CompletedProcess(
            ["whisper-dictate"], 1, stdout="", stderr="boom"
        )
        with mock.patch(
            "whisper_dictate.vp_external_api.subprocess.run", return_value=rust_failure
        ), mock.patch.object(
            vp_external_api,
            "_request_json",
            return_value={"choices": [{"message": {"content": "fallback"}}]},
        ):
            text, _ = vp_external_api.openai_chat_completion(
                base_url="https://api.openai.com/v1",
                api_key="test-key",
                model="gpt-4o-mini",
                prompt="clean this",
                timeout_ms=1000,
            )

        self.assertEqual(text, "fallback")

    def test_rust_helper_skipped_for_empty_api_key(self):
        os.environ["VOICEPI_EXTERNAL_API_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "whisper-dictate"
        from whisper_dictate import vp_external_api

        # Empty key: the Python fallback raises the documented RuntimeError;
        # the Rust helper would also refuse, but skipping it here gives the
        # caller a clearer error message without an extra subprocess hop.
        with mock.patch(
            "whisper_dictate.vp_external_api.subprocess.run"
        ) as run, self.assertRaisesRegex(RuntimeError, "API"):
            vp_external_api.openai_chat_completion(
                base_url="https://api.openai.com/v1",
                api_key="",
                model="gpt-4o-mini",
                prompt="clean",
                timeout_ms=1000,
            )

        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
