"""Tests for the Wave 6 Rust shell-out in runtime._handle_dictionary_training.

The user-facing CLI flags `--dictionary-build-from-corpus` and
`--dictionary-suggest-terms` now shell out to the Rust binary's
`whisper-dictate dictionary build-from-corpus` / `dictionary suggest-terms`
subcommands. When the env-resolved helper is absent OR fails the dispatcher
falls back to the in-process Python path so behaviour never regresses.
"""
from __future__ import annotations

import argparse
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from whisper_dictate import runtime  # noqa: E402


def _ns(**kwargs) -> argparse.Namespace:
    """Build the argparse-style namespace the dispatcher reads from."""
    defaults = {
        "dictionary_build_from_corpus": False,
        "dictionary_suggest_terms": None,
        "dictionary": None,
        "min_count": 1,
        "apply": False,
        "json": False,
        "benchmark_corpus": None,
        "language": None,
        "category": None,
        "app_root": None,
    }
    defaults.update(kwargs)
    return argparse.Namespace(**defaults)


class RustBackendShellOutTests(unittest.TestCase):
    """Drive runtime._handle_dictionary_training through the Rust shell-out."""

    def setUp(self) -> None:
        self._prev_helper = os.environ.get("VOICEPI_RUST_INJECTOR")
        self._prev_backend = os.environ.get("VOICEPI_DICTIONARY_BACKEND")

    def tearDown(self) -> None:
        for key, prev in (
            ("VOICEPI_RUST_INJECTOR", self._prev_helper),
            ("VOICEPI_DICTIONARY_BACKEND", self._prev_backend),
        ):
            if prev is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = prev

    def _install_helper(self, tmp_path: Path) -> Path:
        """Create a sentinel helper file and point the env var at it."""
        helper = tmp_path / ("whisper-dictate.exe" if os.name == "nt" else "whisper-dictate")
        helper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        os.environ["VOICEPI_RUST_INJECTOR"] = str(helper)
        return helper

    # --- env gating ---------------------------------------------------------

    def test_no_helper_env_falls_through_to_python(self):
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)
        rc = runtime._rust_dictionary_training(_ns(dictionary_build_from_corpus=True))
        self.assertIsNone(rc, "no helper env must defer to Python fallback")

    def test_missing_helper_file_falls_through_to_python(self):
        os.environ["VOICEPI_RUST_INJECTOR"] = "/nonexistent/whisper-dictate-binary"
        rc = runtime._rust_dictionary_training(_ns(dictionary_build_from_corpus=True))
        self.assertIsNone(rc)

    # --- argv translation ---------------------------------------------------

    def test_build_from_corpus_argv_carries_every_flag(self):
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=0)
            os.environ["VOICEPI_RUST_INJECTOR"] = "/some/whisper-dictate"
            rc = runtime._rust_dictionary_training(_ns(
                dictionary_build_from_corpus=True,
                benchmark_corpus="corpus.json",
                language="da",
                category="technical",
                dictionary="d.json",
                apply=True,
                json=True,
                min_count=3,
                app_root="/app",
            ))
        self.assertEqual(rc, 0)
        argv = mock_run.call_args.args[0]
        self.assertEqual(argv[0], "/some/whisper-dictate")
        self.assertEqual(argv[1:3], ["dictionary", "build-from-corpus"])
        # Every translated flag must be present (order-insensitive check).
        flat = " ".join(argv[3:])
        self.assertIn("--benchmark-corpus corpus.json", flat)
        self.assertIn("--language da", flat)
        self.assertIn("--category technical", flat)
        self.assertIn("--dictionary d.json", flat)
        self.assertIn("--min-count 3", flat)
        self.assertIn("--app-root /app", flat)
        self.assertIn("--apply", flat)
        self.assertIn("--json", flat)
        # The child must NOT re-enter the dictionary-ops Rust path inside the
        # helper's own logic — we force it back to the Python in-process code.
        env = mock_run.call_args.kwargs["env"]
        self.assertEqual(env.get("VOICEPI_DICTIONARY_BACKEND"), "python")

    def test_suggest_terms_argv_passes_jsonl_positional_and_flags(self):
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=1)
            os.environ["VOICEPI_RUST_INJECTOR"] = "/some/whisper-dictate"
            rc = runtime._rust_dictionary_training(_ns(
                dictionary_suggest_terms="results.jsonl",
                dictionary="d.json",
                apply=True,
                json=True,
                min_count=2,
            ))
        self.assertEqual(rc, 1, "exit code from helper must be relayed verbatim")
        argv = mock_run.call_args.args[0]
        self.assertEqual(argv[1:4], ["dictionary", "suggest-terms", "results.jsonl"])
        flat = " ".join(argv[4:])
        self.assertIn("--dictionary d.json", flat)
        self.assertIn("--min-count 2", flat)
        self.assertIn("--apply", flat)
        self.assertIn("--json", flat)

    def test_helper_exception_returns_none_for_fallback(self):
        with mock.patch.object(runtime.subprocess, "run", side_effect=OSError("boom")), \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            os.environ["VOICEPI_RUST_INJECTOR"] = "/some/whisper-dictate"
            rc = runtime._rust_dictionary_training(_ns(dictionary_build_from_corpus=True))
        self.assertIsNone(rc, "exception in subprocess.run must fall back to Python")

    def test_no_recognised_flag_returns_none(self):
        # Defensive: dispatcher should not be reached, but if it is we report
        # "unknown" so the caller falls back rather than running an empty argv.
        with mock.patch.object(runtime.Path, "exists", return_value=True):
            os.environ["VOICEPI_RUST_INJECTOR"] = "/some/whisper-dictate"
            rc = runtime._rust_dictionary_training(_ns())
        self.assertIsNone(rc)

    # --- dispatcher integration --------------------------------------------

    def test_dispatcher_relays_helper_exit_code(self):
        ap = argparse.ArgumentParser()
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=42)
            os.environ["VOICEPI_RUST_INJECTOR"] = "/some/whisper-dictate"
            rc = runtime._handle_dictionary_training(
                _ns(dictionary_build_from_corpus=True), ap)
        self.assertEqual(rc, 42, "dispatcher must relay the Rust helper's exit code")


if __name__ == "__main__":
    unittest.main()
