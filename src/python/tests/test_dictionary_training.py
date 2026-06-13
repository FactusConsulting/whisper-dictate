"""Unit tests for the pure corpus->dictionary training logic (Feature A)."""
from helpers import unittest

from whisper_dictate import vp_dictionary_training as dt


class ExtractCandidateTermsTests(unittest.TestCase):
    def test_curated_terms_are_always_kept(self):
        cands = dt.extract_candidate_terms(
            ["plain lowercase sentence with nothing special"],
            item_terms=[["Parakeet", "git tag"]],
            item_ids=["a"],
            min_count=5,  # high threshold; curated must survive anyway
        )
        terms = {c.term for c in cands}
        self.assertIn("Parakeet", terms)
        self.assertIn("git tag", terms)

    def test_capitalized_multi_word_run_detected(self):
        cands = dt.extract_candidate_terms(
            ["Jeg tester Claude Code i Windows Terminal."],
            item_ids=["a"],
        )
        terms = {c.term for c in cands}
        self.assertIn("Claude Code", terms)
        self.assertIn("Windows Terminal", terms)

    def test_comma_list_does_not_collapse_into_one_phrase(self):
        cands = dt.extract_candidate_terms(["OpenClaw, MCP, RAG og vLLM."], item_ids=["a"])
        terms = {c.term for c in cands}
        self.assertNotIn("OpenClaw MCP RAG", terms)
        self.assertIn("MCP", terms)
        self.assertIn("RAG", terms)
        self.assertIn("vLLM", terms)

    def test_technical_tokens_detected(self):
        cands = dt.extract_candidate_terms(
            ["Run with large-v3 and the RTX server."],
            item_ids=["a"],
        )
        terms = {c.term for c in cands}
        self.assertIn("large-v3", terms)
        self.assertIn("RTX", terms)

    def test_sentence_initial_stopword_not_a_candidate(self):
        cands = dt.extract_candidate_terms(
            ["Skift backend til parakeet.", "Run the build now."],
            item_ids=["a", "b"],
        )
        terms = {c.term.casefold() for c in cands}
        self.assertNotIn("skift", terms)
        self.assertNotIn("run", terms)

    def test_min_count_filters_one_off_noise(self):
        # "Hetzner" appears once; with min_count=2 it is dropped (non-curated).
        cands = dt.extract_candidate_terms(
            ["Hetzner is mentioned once.", "Kubernetes here.", "Kubernetes again."],
            item_ids=["a", "b", "c"],
            min_count=2,
        )
        terms = {c.term for c in cands}
        self.assertIn("Kubernetes", terms)
        self.assertNotIn("Hetzner", terms)

    def test_sorted_by_count_then_term(self):
        cands = dt.extract_candidate_terms(
            ["Kubernetes", "Kubernetes", "Ansible"],
            item_ids=["a", "b", "c"],
        )
        self.assertEqual(cands[0].term, "Kubernetes")
        self.assertGreaterEqual(cands[0].count, cands[-1].count)

    def test_empty_input_yields_no_candidates(self):
        self.assertEqual(dt.extract_candidate_terms([]), [])

    def test_samples_capture_item_ids(self):
        cands = dt.extract_candidate_terms(
            ["Kubernetes cluster."],
            item_terms=[["Kubernetes"]],
            item_ids=["da-tech-003"],
        )
        kube = next(c for c in cands if c.term == "Kubernetes")
        self.assertIn("da-tech-003", kube.samples)


class MergeTermsTests(unittest.TestCase):
    def test_append_new_terms(self):
        preview = dt.merge_terms(["Existing"], ["New", "Another"])
        self.assertEqual(preview.added, ["New", "Another"])
        self.assertEqual(preview.result_terms, ["Existing", "New", "Another"])
        self.assertEqual(preview.existing_count, 1)

    def test_dedup_case_insensitive_against_existing(self):
        preview = dt.merge_terms(["Claude Code"], ["claude code", "Codex"])
        self.assertEqual(preview.added, ["Codex"])
        self.assertIn("claude code", preview.skipped_existing)

    def test_dedup_within_candidates(self):
        preview = dt.merge_terms([], ["MCP", "mcp", "RAG"])
        self.assertEqual(preview.added, ["MCP", "RAG"])

    def test_accepts_term_candidate_objects(self):
        cands = [dt.TermCandidate(term="Parakeet"), dt.TermCandidate(term="Codex")]
        preview = dt.merge_terms(["Codex"], cands)
        self.assertEqual(preview.added, ["Parakeet"])

    def test_blank_candidates_ignored(self):
        preview = dt.merge_terms([], ["  ", "", "Real"])
        self.assertEqual(preview.added, ["Real"])

    def test_existing_order_preserved(self):
        preview = dt.merge_terms(["B", "A"], ["C"])
        self.assertEqual(preview.result_terms, ["B", "A", "C"])

    def test_no_candidates_means_no_change(self):
        preview = dt.merge_terms(["A"], [])
        self.assertEqual(preview.added, [])
        self.assertEqual(preview.result_terms, ["A"])


class SuggestFromMissesTests(unittest.TestCase):
    def _rows(self):
        return [
            {"corpus_id": "da-tech-004", "term_misses": ["merge", "deploy"]},
            {"corpus_id": "da-tech-001", "term_misses": ["merge"]},
            {"corpus_id": "en-tech-002", "term_misses": ["NVIDIA Parakeet"]},
            {"corpus_id": "x", "term_misses": []},
            {"corpus_id": "y"},  # no term_misses key at all
        ]

    def test_counts_and_sorts_misses(self):
        sugg = dt.suggest_terms_from_misses(self._rows())
        by_term = {s.term: s for s in sugg}
        self.assertEqual(by_term["merge"].count, 2)
        self.assertEqual(by_term["deploy"].count, 1)
        # Highest count first.
        self.assertEqual(sugg[0].term, "merge")

    def test_flags_terms_already_in_dictionary(self):
        sugg = dt.suggest_terms_from_misses(self._rows(), existing_terms=["Deploy"])
        deploy = next(s for s in sugg if s.term == "deploy")
        self.assertTrue(deploy.already_in_dictionary)
        merge = next(s for s in sugg if s.term == "merge")
        self.assertFalse(merge.already_in_dictionary)

    def test_min_count_filters(self):
        sugg = dt.suggest_terms_from_misses(self._rows(), min_count=2)
        terms = {s.term for s in sugg}
        self.assertEqual(terms, {"merge"})

    def test_samples_record_corpus_ids(self):
        sugg = dt.suggest_terms_from_misses(self._rows())
        merge = next(s for s in sugg if s.term == "merge")
        self.assertIn("da-tech-004", merge.samples)

    def test_string_term_misses_field_is_tolerated(self):
        sugg = dt.suggest_terms_from_misses([{"corpus_id": "z", "term_misses": "merge"}])
        self.assertEqual([s.term for s in sugg], ["merge"])

    def test_empty_rows_yield_no_suggestions(self):
        self.assertEqual(dt.suggest_terms_from_misses([]), [])
