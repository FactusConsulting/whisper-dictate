"""Tests for the Rust shell-out that owns dictionary training / suggestions.

Audit item 4 (``docs/architecture-audit-2026-07-16.md``) retired the Python
parity implementations (`vp_dictionary_training.py`, `vp_dictionary_suggest.py`,
`vp_dictionary_training_cli.py`). The three user-facing flags on the Python
CLI:

* ``--dictionary-suggest`` — fuzzy replacement suggestion
* ``--dictionary-build-from-corpus`` — corpus->dictionary term mining
* ``--dictionary-suggest-terms`` — term additions from benchmark misses

now shell out to ``whisper-dictate dictionary suggest-replacements`` /
``build-from-corpus`` / ``suggest-terms`` respectively. There is no Python
in-process fallback anymore: a missing / unreachable Rust binary is a hard
error surfaced through ``argparse.error`` (exit 2).
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
        "dictionary_suggest": None,
        "dictionary_suggest_min_confidence": 0.62,
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


class _EnvSnapshot(unittest.TestCase):
    """Snapshot + restore ``VOICEPI_RUST_INJECTOR`` for every case."""

    KEY = "VOICEPI_RUST_INJECTOR"

    def setUp(self) -> None:
        self._prev = os.environ.get(self.KEY)

    def tearDown(self) -> None:
        if self._prev is None:
            os.environ.pop(self.KEY, None)
        else:
            os.environ[self.KEY] = self._prev


class RustDictionarySubcommandTests(_EnvSnapshot):
    """``_rust_dictionary_subcommand`` gate + subprocess launch behaviour."""

    def test_no_helper_env_returns_none(self):
        os.environ.pop(self.KEY, None)
        self.assertIsNone(runtime._rust_dictionary_subcommand(["build-from-corpus"]))

    def test_missing_helper_file_returns_none(self):
        os.environ[self.KEY] = "/nonexistent/whisper-dictate-binary"
        self.assertIsNone(runtime._rust_dictionary_subcommand(["build-from-corpus"]))

    def test_subprocess_launch_failure_returns_none(self):
        with mock.patch.object(runtime.subprocess, "run", side_effect=OSError("boom")), \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            os.environ[self.KEY] = "/some/whisper-dictate"
            self.assertIsNone(runtime._rust_dictionary_subcommand(["build-from-corpus"]))

    def test_successful_launch_relays_exit_code(self):
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=7)
            os.environ[self.KEY] = "/some/whisper-dictate"
            rc = runtime._rust_dictionary_subcommand(["suggest-terms", "results.jsonl"])
        self.assertEqual(rc, 7)
        argv = mock_run.call_args.args[0]
        self.assertEqual(argv[0], "/some/whisper-dictate")
        self.assertEqual(argv[1:], ["dictionary", "suggest-terms", "results.jsonl"])


class DictionaryTrainingArgvTests(unittest.TestCase):
    """The argv translation must carry every recognised flag verbatim."""

    def test_build_from_corpus_argv_carries_every_flag(self):
        args = runtime._rust_dictionary_training_args(_ns(
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
        self.assertIsNotNone(args)
        flat = " ".join(args)
        self.assertTrue(args[0] == "build-from-corpus")
        self.assertIn("--benchmark-corpus corpus.json", flat)
        self.assertIn("--language da", flat)
        self.assertIn("--category technical", flat)
        self.assertIn("--dictionary d.json", flat)
        self.assertIn("--min-count 3", flat)
        self.assertIn("--app-root /app", flat)
        self.assertIn("--apply", flat)
        self.assertIn("--json", flat)

    def test_suggest_terms_argv_carries_positional_and_flags(self):
        args = runtime._rust_dictionary_training_args(_ns(
            dictionary_suggest_terms="results.jsonl",
            dictionary="d.json",
            apply=True,
            json=True,
            min_count=2,
        ))
        self.assertIsNotNone(args)
        self.assertEqual(args[:2], ["suggest-terms", "results.jsonl"])
        flat = " ".join(args[2:])
        self.assertIn("--dictionary d.json", flat)
        self.assertIn("--min-count 2", flat)
        self.assertIn("--apply", flat)
        self.assertIn("--json", flat)

    def test_no_recognised_flag_returns_none(self):
        # Defensive: dispatcher should only call this when one of the flags
        # is set, but explicit None keeps a bug from silently running an
        # empty subcommand.
        self.assertIsNone(runtime._rust_dictionary_training_args(_ns()))

    def test_min_count_one_is_omitted(self):
        # Rust default is 1 → don't clutter argv when the user did not
        # override.
        args = runtime._rust_dictionary_training_args(_ns(
            dictionary_build_from_corpus=True,
            min_count=1,
        ))
        self.assertNotIn("--min-count", args)


class DictionaryTrainingDispatchTests(_EnvSnapshot):
    """``_handle_dictionary_training`` requires the Rust binary post audit 4."""

    def test_success_relays_helper_exit_code(self):
        ap = argparse.ArgumentParser()
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=42)
            os.environ[self.KEY] = "/some/whisper-dictate"
            rc = runtime._handle_dictionary_training(
                _ns(dictionary_build_from_corpus=True), ap)
        self.assertEqual(rc, 42)

    def test_missing_binary_is_hard_error(self):
        ap = argparse.ArgumentParser()
        os.environ.pop(self.KEY, None)
        with self.assertRaises(SystemExit):
            runtime._handle_dictionary_training(
                _ns(dictionary_build_from_corpus=True), ap)


class DictionarySuggestDispatchTests(_EnvSnapshot):
    """``_handle_dictionary_suggest`` shell-out shape + hard-fail."""

    def test_success_relays_helper_exit_code_and_translates_argv(self):
        ap = argparse.ArgumentParser()
        with mock.patch.object(runtime.subprocess, "run") as mock_run, \
                mock.patch.object(runtime.Path, "exists", return_value=True):
            mock_run.return_value = mock.MagicMock(returncode=0)
            os.environ[self.KEY] = "/some/whisper-dictate"
            rc = runtime._handle_dictionary_suggest(
                _ns(
                    dictionary_suggest="rows.jsonl",
                    dictionary_suggest_min_confidence=0.55,
                    dictionary="d.json",
                    json=True,
                ),
                ap,
            )
        self.assertEqual(rc, 0)
        argv = mock_run.call_args.args[0]
        self.assertEqual(argv[1:3], ["dictionary", "suggest-replacements"])
        self.assertEqual(argv[3], "rows.jsonl")
        flat = " ".join(argv[4:])
        self.assertIn("--min-confidence 0.55", flat)
        self.assertIn("--dictionary d.json", flat)
        self.assertIn("--json", flat)

    def test_missing_binary_is_hard_error(self):
        ap = argparse.ArgumentParser()
        os.environ.pop(self.KEY, None)
        with self.assertRaises(SystemExit):
            runtime._handle_dictionary_suggest(
                _ns(dictionary_suggest="rows.jsonl"), ap)


if __name__ == "__main__":
    unittest.main()
