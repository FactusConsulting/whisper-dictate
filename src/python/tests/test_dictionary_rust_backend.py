"""Tests for ``VOICEPI_DICTIONARY_BACKEND=rust`` shell-out in vp_dictionary_*.

Wave 4-A of the Python-removal roadmap (#348). The Rust ops dispatcher lives
in ``src/rust/dictionary/ops.rs`` and is reached via
``whisper-dictate dictionary-ops``; this file exercises:

* env-var gating (unset env -> Python path, set env -> shell-out)
* missing helper / non-zero exit / invalid JSON -> graceful Python fallback
* successful shell-out -> Rust response decoded into the dataclass shape
  the Python callers expect.

The Rust binary is NOT invoked — we mock ``subprocess.run`` so the test runs
on machines without a built Rust binary (CI's Python lane).
"""
from __future__ import annotations

import json
import os
import sys
import unittest
from unittest import mock


HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from whisper_dictate import vp_dictionary_suggest, vp_dictionary_training  # noqa: E402


def _fake_completed(*, returncode: int = 0, stdout: str = "", stderr: str = ""):
    """Stand-in for ``subprocess.CompletedProcess`` with the attrs we touch."""
    return mock.MagicMock(returncode=returncode, stdout=stdout, stderr=stderr)


class _EnvSnapshot(unittest.TestCase):
    """Mixin: snapshot + restore the two env vars every shell-out path uses."""

    KEYS = ("VOICEPI_DICTIONARY_BACKEND", "VOICEPI_RUST_INJECTOR")

    def setUp(self) -> None:  # noqa: D401 - inherited setUp hook
        super().setUp()
        self._prev = {key: os.environ.get(key) for key in self.KEYS}

    def tearDown(self) -> None:  # noqa: D401 - inherited tearDown hook
        for key, prev in self._prev.items():
            if prev is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = prev
        super().tearDown()


class ExtractCandidateTermsShellOutTests(_EnvSnapshot):
    def test_unset_env_falls_through_to_python(self) -> None:
        os.environ.pop("VOICEPI_DICTIONARY_BACKEND", None)
        with mock.patch.object(vp_dictionary_training.subprocess, "run") as run:
            cands = vp_dictionary_training.extract_candidate_terms(
                ["Kubernetes runs Kubernetes."],
                item_ids=["a"],
            )
        run.assert_not_called()
        self.assertTrue(any(c.term == "Kubernetes" for c in cands))

    def test_env_without_helper_path_falls_through(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)
        with mock.patch.object(vp_dictionary_training.subprocess, "run") as run:
            cands = vp_dictionary_training.extract_candidate_terms(
                ["Kubernetes."], item_ids=["a"]
            )
        run.assert_not_called()
        self.assertTrue(any(c.term == "Kubernetes" for c in cands))

    def test_helper_failure_falls_through(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(returncode=2, stderr="oops"),
        ):
            cands = vp_dictionary_training.extract_candidate_terms(
                ["Kubernetes."], item_ids=["a"]
            )
        self.assertTrue(any(c.term == "Kubernetes" for c in cands))

    def test_helper_invalid_json_falls_through(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(stdout="{not json"),
        ):
            cands = vp_dictionary_training.extract_candidate_terms(
                ["Kubernetes."], item_ids=["a"]
            )
        self.assertTrue(any(c.term == "Kubernetes" for c in cands))

    def test_successful_shell_out_decodes_rust_payload(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        payload = {
            "candidates": [
                {
                    "term": "Parakeet",
                    "count": 3,
                    "reason": "curated_term",
                    "samples": ["a", "b"],
                },
                {"term": "MCP", "count": 1, "reason": "technical", "samples": []},
            ]
        }
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(stdout=json.dumps(payload)),
        ) as run:
            cands = vp_dictionary_training.extract_candidate_terms(
                ["irrelevant input — Rust will be trusted"],
                item_terms=[["Parakeet"]],
                item_ids=["a"],
            )
        run.assert_called_once()
        terms = [c.term for c in cands]
        self.assertEqual(terms, ["Parakeet", "MCP"])
        parakeet = cands[0]
        self.assertEqual(parakeet.count, 3)
        self.assertEqual(parakeet.reason, "curated_term")
        self.assertEqual(parakeet.samples, ("a", "b"))


class MergeTermsShellOutTests(_EnvSnapshot):
    def test_successful_shell_out_returns_rust_preview(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        payload = {
            "added": ["New"],
            "skipped_existing": ["Old"],
            "result_terms": ["Old", "New"],
            "existing_count": 1,
            "added_count": 1,
        }
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(stdout=json.dumps(payload)),
        ):
            preview = vp_dictionary_training.merge_terms(["Old"], ["New", "old"])
        self.assertEqual(preview.added, ["New"])
        self.assertEqual(preview.skipped_existing, ["Old"])
        self.assertEqual(preview.existing_count, 1)

    def test_failure_falls_through_to_python(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(returncode=1, stderr="boom"),
        ):
            preview = vp_dictionary_training.merge_terms(["Old"], ["New"])
        self.assertEqual(preview.added, ["New"])
        self.assertEqual(preview.result_terms, ["Old", "New"])


class SuggestTermsFromMissesShellOutTests(_EnvSnapshot):
    def test_successful_shell_out_returns_rust_suggestions(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        payload = {
            "suggestions": [
                {
                    "term": "merge",
                    "count": 2,
                    "samples": ["a"],
                    "already_in_dictionary": False,
                },
                {
                    "term": "deploy",
                    "count": 1,
                    "samples": [],
                    "already_in_dictionary": True,
                },
            ]
        }
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(stdout=json.dumps(payload)),
        ):
            sugg = vp_dictionary_training.suggest_terms_from_misses(
                [{"corpus_id": "a", "term_misses": ["merge"]}]
            )
        self.assertEqual([s.term for s in sugg], ["merge", "deploy"])
        self.assertTrue(sugg[1].already_in_dictionary)

    def test_failure_falls_through_to_python(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictionary_training.subprocess,
            "run",
            return_value=_fake_completed(returncode=1),
        ):
            sugg = vp_dictionary_training.suggest_terms_from_misses(
                [{"corpus_id": "a", "term_misses": ["merge"]}]
            )
        self.assertEqual([s.term for s in sugg], ["merge"])


class SuggestReplacementsShellOutTests(_EnvSnapshot):
    def setUp(self) -> None:
        super().setUp()
        # Snapshot DICTIONARY so the suggester sees an empty live state.
        self._prev_terms = vp_dictionary_suggest.DICTIONARY.terms
        self._prev_repl = vp_dictionary_suggest.DICTIONARY.replacements
        vp_dictionary_suggest.DICTIONARY.terms = []
        vp_dictionary_suggest.DICTIONARY.replacements = {}

    def tearDown(self) -> None:
        vp_dictionary_suggest.DICTIONARY.terms = self._prev_terms
        vp_dictionary_suggest.DICTIONARY.replacements = self._prev_repl
        super().tearDown()

    def test_unset_env_falls_through_to_python(self) -> None:
        os.environ.pop("VOICEPI_DICTIONARY_BACKEND", None)
        with mock.patch.object(
            vp_dictionary_suggest.subprocess, "run"
        ) as run:
            vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "", "corpus_id": "a"}], min_confidence=0.9
            )
        run.assert_not_called()

    def test_successful_shell_out_decodes_payload(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        payload = {
            "suggestions": [
                {
                    "source": "Murch",
                    "target": "Merge",
                    "count": 2,
                    "confidence": 0.85,
                    "reason": "term_miss_fuzzy_match",
                    "samples": ["da-tech-004"],
                }
            ]
        }
        with mock.patch.object(
            vp_dictionary_suggest.subprocess,
            "run",
            return_value=_fake_completed(stdout=json.dumps(payload)),
        ):
            out = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "Murch", "term_misses": ["Merge"]}], min_confidence=0.55
            )
        self.assertEqual(len(out), 1)
        suggestion = out[0]
        self.assertEqual(suggestion.source, "Murch")
        self.assertEqual(suggestion.target, "Merge")
        self.assertEqual(suggestion.count, 2)
        self.assertAlmostEqual(suggestion.confidence, 0.85)
        self.assertEqual(suggestion.samples, ["da-tech-004"])

    def test_failure_falls_through_to_python(self) -> None:
        os.environ["VOICEPI_DICTIONARY_BACKEND"] = "rust"
        os.environ["VOICEPI_RUST_INJECTOR"] = "/fake/whisper-dictate"
        with mock.patch.object(
            vp_dictionary_suggest.subprocess,
            "run",
            return_value=_fake_completed(returncode=1),
        ):
            # Python path executes and returns at minimum []; we only assert no
            # crash, not a specific list (the Python heuristic is exercised by
            # other tests in test_dictionary_suggest.py).
            out = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "", "corpus_id": "a"}], min_confidence=0.9
            )
        self.assertIsInstance(out, list)


if __name__ == "__main__":  # pragma: no cover - convenience runner
    unittest.main()
