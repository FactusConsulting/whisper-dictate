"""Unit tests for the pure corpus filter-profiles (Feature B)."""
from helpers import unittest

from whisper_dictate import vp_corpus_profile as cp


class _Item:
    """Lightweight corpus-item double (matches the language/category Protocol)."""

    def __init__(self, language: str, category: str, id_: str = "") -> None:
        self.language = language
        self.category = category
        self.id = id_


def _corpus() -> list[_Item]:
    return [
        _Item("da", "mixed_technical", "da-tech-001"),
        _Item("da", "short_danish", "da-short-001"),
        _Item("da", "terminal", "da-term-001"),
        _Item("en", "english_technical", "en-tech-001"),
        _Item("en", "short_english", "en-short-001"),
        _Item("da", "product_names", "da-prod-001"),
    ]


class BuildProfileTests(unittest.TestCase):
    def test_empty_profile_selects_all(self):
        profile = cp.build_profile()
        self.assertTrue(profile.is_empty)
        items = _corpus()
        self.assertEqual(cp.filter_corpus_items(items, profile), items)

    def test_blank_strings_yield_empty_profile(self):
        profile = cp.build_profile(language="  ", category=" , ")
        self.assertTrue(profile.is_empty)

    def test_language_filter_single(self):
        profile = cp.build_profile(language="da")
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertEqual({i.language for i in got}, {"da"})
        self.assertEqual(len(got), 4)

    def test_language_filter_is_case_insensitive(self):
        profile = cp.build_profile(language="DA")
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertTrue(got)
        self.assertEqual({i.language for i in got}, {"da"})

    def test_language_filter_comma_separated(self):
        profile = cp.build_profile(language="da,en")
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertEqual(len(got), len(_corpus()))

    def test_category_group_expands_to_real_categories(self):
        profile = cp.build_profile(category="technical")
        self.assertEqual(
            set(profile.categories),
            {"mixed_technical", "english_technical", "terminal"},
        )
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertEqual(
            {i.category for i in got},
            {"mixed_technical", "english_technical", "terminal"},
        )

    def test_exact_category_name_still_works(self):
        profile = cp.build_profile(category="product_names")
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertEqual([i.id for i in got], ["da-prod-001"])

    def test_language_and_category_are_anded(self):
        profile = cp.build_profile(language="da", category="technical")
        got = cp.filter_corpus_items(_corpus(), profile)
        self.assertEqual({i.category for i in got}, {"mixed_technical", "terminal"})
        self.assertTrue(all(i.language == "da" for i in got))

    def test_unknown_selectors_match_nothing(self):
        profile = cp.build_profile(language="fr", category="nonsense")
        self.assertEqual(cp.filter_corpus_items(_corpus(), profile), [])

    def test_describe_is_readable(self):
        self.assertEqual(cp.build_profile().describe(), "all items")
        self.assertIn("language=da", cp.build_profile(language="da").describe())

    def test_filter_preserves_order(self):
        profile = cp.build_profile(language="da")
        items = _corpus()
        got = cp.filter_corpus_items(items, profile)
        self.assertEqual([i.id for i in got], [i.id for i in items if i.language == "da"])

    def test_known_languages_and_categories(self):
        items = _corpus()
        self.assertEqual(cp.known_languages(items), ["da", "en"])
        self.assertIn("terminal", cp.known_categories(items))


class RealCorpusProfileTests(unittest.TestCase):
    """Profiles must map to the categories actually present in benchmark/corpus.json."""

    def _load_real_corpus(self):
        import json
        from pathlib import Path

        manifest = Path(__file__).resolve().parents[3] / "benchmark" / "corpus.json"
        if not manifest.exists():
            self.skipTest("benchmark/corpus.json not found")
        data = json.loads(manifest.read_text(encoding="utf-8"))
        return [_Item(it.get("language", ""), it.get("category", ""), it.get("id", ""))
                for it in data["items"]]

    def test_technical_group_matches_real_corpus(self):
        items = self._load_real_corpus()
        profile = cp.build_profile(category="technical")
        got = cp.filter_corpus_items(items, profile)
        self.assertTrue(got, "technical group should match at least one real item")

    def test_da_language_matches_real_corpus(self):
        items = self._load_real_corpus()
        got = cp.filter_corpus_items(items, cp.build_profile(language="da"))
        self.assertTrue(got)
        self.assertTrue(all(i.language == "da" for i in got))
