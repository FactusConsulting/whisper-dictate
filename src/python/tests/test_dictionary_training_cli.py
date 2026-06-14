"""Integration-ish tests for the dictionary-training CLI orchestration.

Exercises the build-from-corpus and suggest-from-misses orchestration against a
temp corpus manifest + temp dictionary, asserting the preview/apply safety and the
JSON contract. The heavy STT path is never touched (these read TEXT only).
"""
from helpers import (
    _capture_stdout,
    json,
    Path,
    sys,
    tempfile,
    unittest,
)

from whisper_dictate import vp_dictionary_training_cli as cli


def _write_corpus(dir_: Path) -> Path:
    manifest = dir_ / "corpus.json"
    manifest.write_text(json.dumps({
        "version": 1,
        "audio_dir": "audio",
        "items": [
            {"id": "da-tech-001", "language": "da", "category": "mixed_technical",
             "text": "Skift backend til Parakeet og behold dictionary replacements.",
             "terms": ["Parakeet", "dictionary", "replacements"]},
            {"id": "en-short-001", "language": "en", "category": "short_english",
             "text": "Please check the latest build.", "terms": []},
            {"id": "da-prod-001", "language": "da", "category": "product_names",
             "text": "Claude Code og Codex skal forstå prompten.",
             "terms": ["Claude Code", "Codex"]},
        ],
    }), encoding="utf-8")
    return manifest


class BuildFromCorpusCliTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.manifest = _write_corpus(self.dir)
        self.dict_path = self.dir / "dictionary.json"

    def test_preview_does_not_write(self):
        with _capture_stdout() as buf:
            rc = cli.run_build_from_corpus(
                corpus_manifest=self.manifest,
                dictionary_path=self.dict_path,
                apply=False,
            )
        self.assertEqual(rc, 0)
        self.assertFalse(self.dict_path.exists())
        out = buf.getvalue()
        self.assertIn("PREVIEW only", out)
        self.assertIn("never records audio", out)

    def test_apply_writes_terms(self):
        rc = cli.run_build_from_corpus(
            corpus_manifest=self.manifest,
            dictionary_path=self.dict_path,
            apply=True,
        )
        self.assertEqual(rc, 0)
        self.assertTrue(self.dict_path.exists())
        terms = json.loads(self.dict_path.read_text(encoding="utf-8"))["terms"]
        self.assertIn("Parakeet", terms)
        self.assertIn("Claude Code", terms)

    def test_profile_restricts_to_subset(self):
        with _capture_stdout() as buf:
            cli.run_build_from_corpus(
                corpus_manifest=self.manifest,
                dictionary_path=self.dict_path,
                language="da",
                category="product_names",
                apply=False,
                as_json=True,
            )
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertEqual(payload["corpus_items"], 1)
        self.assertIn("Claude Code", payload["added"])
        self.assertNotIn("Parakeet", payload["added"])

    def test_rerun_after_apply_adds_nothing(self):
        cli.run_build_from_corpus(
            corpus_manifest=self.manifest, dictionary_path=self.dict_path, apply=True)
        with _capture_stdout() as buf:
            cli.run_build_from_corpus(
                corpus_manifest=self.manifest, dictionary_path=self.dict_path,
                apply=True, as_json=True)
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertEqual(payload["added"], [])
        self.assertFalse(payload["applied"])

    def test_missing_corpus_reports_error(self):
        with _capture_stdout() as buf:
            rc = cli.run_build_from_corpus(
                corpus_manifest=self.dir / "does_not_exist.json",
                dictionary_path=self.dict_path,
                as_json=True,
            )
        self.assertEqual(rc, 1)
        self.assertIn("error", json.loads(buf.getvalue().strip().splitlines()[-1]))

    def test_invalid_corpus_json_returns_one(self):
        # A corpus file with broken JSON makes load_corpus raise ValueError/
        # JSONDecodeError; the command must catch it and return 1 (clean error),
        # not let it bubble to argparse's exit 2.
        bad = self.dir / "bad_corpus.json"
        bad.write_text("{ not valid json", encoding="utf-8")
        with _capture_stdout() as buf:
            rc = cli.run_build_from_corpus(
                corpus_manifest=bad,
                dictionary_path=self.dict_path,
                as_json=True,
            )
        self.assertEqual(rc, 1)
        self.assertIn("error", json.loads(buf.getvalue().strip().splitlines()[-1]))

    def test_zero_match_profile_reports_clear_error(self):
        # #272: a SPECIFIED profile that matches no corpus items must fail clearly
        # (rc 1 + clear message) rather than silently producing an empty build.
        with _capture_stdout() as buf:
            rc = cli.run_build_from_corpus(
                corpus_manifest=self.manifest,
                dictionary_path=self.dict_path,
                language="fr",            # no French items in the corpus
                as_json=True,
            )
        self.assertEqual(rc, 1)
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertIn("no corpus items matched profile", payload["error"])
        self.assertFalse(self.dict_path.exists())  # nothing written

    def test_empty_dictionary_path_reports_clean_error(self):
        # #272: --dictionary "" is rejected with a clean error (rc 1 + JSON
        # error), routed through the command's _fail — not a raw traceback.
        with _capture_stdout() as buf:
            rc = cli.run_build_from_corpus(
                corpus_manifest=self.manifest,
                dictionary_path="",
                as_json=True,
            )
        self.assertEqual(rc, 1)
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertIn("dictionary path is empty", payload["error"])

    def test_explicit_corpus_path_with_env_var_is_expanded(self):
        # An explicit manifest given with a $VAR is expanded before the existence
        # check (the resolver returns the explicit path verbatim).
        import os
        old = os.environ.get("WD_TEST_CORPUS_DIR")
        os.environ["WD_TEST_CORPUS_DIR"] = str(self.dir)
        try:
            with _capture_stdout() as buf:
                rc = cli.run_build_from_corpus(
                    corpus_manifest="$WD_TEST_CORPUS_DIR/corpus.json",
                    dictionary_path=self.dict_path,
                    apply=False,
                    as_json=True,
                )
            self.assertEqual(rc, 0)
            payload = json.loads(buf.getvalue().strip().splitlines()[-1])
            self.assertEqual(payload["corpus_items"], 3)
        finally:
            if old is None:
                os.environ.pop("WD_TEST_CORPUS_DIR", None)
            else:
                os.environ["WD_TEST_CORPUS_DIR"] = old

    def test_missing_explicit_corpus_is_listed_in_error(self):
        # The explicit (expanded) path appears in the "looked" list so the error is
        # actionable, not just the implicit search dirs.
        explicit = self.dir / "nope.json"
        with _capture_stdout() as buf:
            cli.run_build_from_corpus(
                corpus_manifest=explicit,
                dictionary_path=self.dict_path,
                as_json=True,
            )
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertIn("nope.json", payload["error"])


class SuggestFromMissesCliTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.dict_path = self.dir / "dictionary.json"
        self.dict_path.write_text(json.dumps(
            {"terms": ["deploy"], "replacements": {}}), encoding="utf-8")
        self.jsonl = self.dir / "results.jsonl"
        self.jsonl.write_text(
            "\n".join([
                json.dumps({"corpus_id": "da-tech-004", "term_misses": ["merge", "deploy"]}),
                json.dumps({"corpus_id": "en-tech-002", "term_misses": ["NVIDIA Parakeet"]}),
                "",
                "not-json",
            ]),
            encoding="utf-8",
        )

    def test_preview_lists_new_and_existing(self):
        with _capture_stdout() as buf:
            rc = cli.run_suggest_from_misses(
                self.jsonl, dictionary_path=self.dict_path, as_json=True)
        self.assertEqual(rc, 0)
        payload = json.loads(buf.getvalue().strip().splitlines()[-1])
        self.assertFalse(payload["applied"])
        self.assertIn("NVIDIA Parakeet", payload["new_terms"])
        self.assertNotIn("deploy", payload["new_terms"])  # already present
        # dictionary unchanged on preview
        terms = json.loads(self.dict_path.read_text(encoding="utf-8"))["terms"]
        self.assertEqual(terms, ["deploy"])

    def test_apply_adds_only_new_terms(self):
        cli.run_suggest_from_misses(
            self.jsonl, dictionary_path=self.dict_path, apply=True)
        terms = json.loads(self.dict_path.read_text(encoding="utf-8"))["terms"]
        self.assertIn("merge", terms)
        self.assertIn("NVIDIA Parakeet", terms)
        self.assertEqual(terms.count("deploy"), 1)  # not duplicated

    def test_missing_jsonl_reports_error(self):
        with _capture_stdout() as buf:
            rc = cli.run_suggest_from_misses(
                self.dir / "missing.jsonl", dictionary_path=self.dict_path, as_json=True)
        self.assertEqual(rc, 1)
        self.assertIn("error", json.loads(buf.getvalue().strip().splitlines()[-1]))


class ParserTests(unittest.TestCase):
    def test_parser_accepts_training_flags(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--dictionary-build-from-corpus",
            "--language", "da",
            "--category", "technical",
            "--dictionary", "d.json",
            "--apply",
            "--min-count", "2",
        ])
        self.assertTrue(args.dictionary_build_from_corpus)
        self.assertEqual(args.language, "da")
        self.assertEqual(args.category, "technical")
        self.assertEqual(args.dictionary, "d.json")
        self.assertTrue(args.apply)
        self.assertEqual(args.min_count, 2)

    def test_parser_accepts_suggest_terms_flag(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--dictionary-suggest-terms", "results.jsonl",
        ])
        self.assertEqual(args.dictionary_suggest_terms, "results.jsonl")
