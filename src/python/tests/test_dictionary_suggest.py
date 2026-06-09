from helpers import (
    _capture_stdout,
    json,
    os,
    patch,
    Path,
    real_numpy,
    sys,
    tempfile,
    types,
    unittest,
    wave,
)

class DictionarySuggestTests(unittest.TestCase):
    def test_suggests_replacements_from_benchmark_term_misses(self):
        from whisper_dictate import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        rows = [{
            "corpus_id": "da-tech-004",
            "text": "Murch branchedes og de plåede den nye version bagefter.",
            "reference_text": "Merge branchen og deploy den nye version bagefter.",
            "term_misses": ["merge", "deploy"],
            "reference_terms": ["merge", "deploy"],
        }]

        try:
            vp_dictionary_suggest.DICTIONARY.terms = []
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                rows, min_confidence=0.55)
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        pairs = {(s.source.casefold(), s.target.casefold()) for s in suggestions}
        self.assertIn(("murch", "merge"), pairs)
        self.assertNotIn(("de", "deploy"), pairs)

    def test_suggest_filters_common_word_sources(self):
        from whisper_dictate import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        rows = [{
            "corpus_id": "sample",
            "text": "og de til le as skal Code Large MCP Claude Set",
            "reference_text": "vLLM deploy type Codex bullets Hetzner Codex large v3",
            "term_misses": [
                "vLLM", "deploy", "type", "Codex", "bullets", "Hetzner",
                "large v3", "RAG", "Claude Code", "STT",
            ],
        }]

        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["MCP", "Claude"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                rows, min_confidence=0.55)
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        sources = {s.source.casefold() for s in suggestions}
        self.assertFalse(sources & {
            "og", "de", "til", "le", "as", "skal", "code", "large",
            "mcp", "claude", "set", "mig", "køre", "typ",
        })

    def test_suggests_dictionary_term_near_misses(self):
        from whisper_dictate import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["Claude Code"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "Clort kode should work", "corpus_id": "sample"}],
                min_confidence=0.45,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertTrue(any(s.target == "Claude Code" for s in suggestions))

    def test_benchmark_rows_without_misses_do_not_scan_whole_dictionary(self):
        from whisper_dictate import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["AMD"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{
                    "text": "and then continue",
                    "reference_text": "and then continue",
                    "reference_terms": [],
                    "term_misses": [],
                }],
                min_confidence=0.5,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertEqual(suggestions, [])

    def test_suggest_does_not_duplicate_existing_replacements(self):
        from whisper_dictate import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["lead dev"]
            vp_dictionary_suggest.DICTIONARY.replacements = {"lead death": "lead dev"}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "lead death", "term_misses": ["lead dev"]}],
                min_confidence=0.5,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertFalse(any(s.source == "lead death" and s.target == "lead dev"
                             for s in suggestions))

    def test_parser_accepts_dictionary_suggest(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--dictionary-suggest", "benchmark/results.jsonl",
            "--dictionary-suggest-min-confidence", "0.7",
        ])

        self.assertEqual(args.dictionary_suggest, "benchmark/results.jsonl")
        self.assertEqual(args.dictionary_suggest_min_confidence, 0.7)



class DictionaryParseReplacementsTests(unittest.TestCase):
    def test_parse_replacements_extracts_from_to_and_skips_invalid(self):
        from whisper_dictate import vp_dictionary_suggest as m
        payload = {"replacements": [
            {"from": "Murch", "to": "Merge"},
            "not-a-dict",
            {"from": "", "to": "x"},
            {"from": "a", "to": ""},
            {"from": " plåede ", "to": " deploy "},
        ]}
        self.assertEqual(
            m._parse_replacements(payload),
            {"Murch": "Merge", "plåede": "deploy"},
        )

    def test_parse_replacements_empty_without_key(self):
        from whisper_dictate import vp_dictionary_suggest as m
        self.assertEqual(m._parse_replacements({}), {})
