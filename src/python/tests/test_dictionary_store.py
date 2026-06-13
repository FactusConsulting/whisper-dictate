"""Unit tests for the dictionary.json IO layer (path resolve + load/write)."""
from helpers import (
    json,
    os,
    Path,
    tempfile,
    unittest,
)

from whisper_dictate import vp_dictionary_store as store


class ResolveDictionaryPathTests(unittest.TestCase):
    def test_explicit_path_wins(self):
        self.assertEqual(
            store.resolve_dictionary_path("/tmp/custom.json"),
            Path("/tmp/custom.json"),
        )

    def test_env_path_used_when_no_explicit(self):
        old = os.environ.get("VOICEPI_DICTIONARY")
        try:
            os.environ["VOICEPI_DICTIONARY"] = os.pathsep.join(["/tmp/a.json", "/tmp/b.json"])
            # First entry of a pathsep list is the write target.
            self.assertEqual(store.resolve_dictionary_path(), Path("/tmp/a.json"))
        finally:
            if old is None:
                os.environ.pop("VOICEPI_DICTIONARY", None)
            else:
                os.environ["VOICEPI_DICTIONARY"] = old

    def test_default_when_nothing_set(self):
        old = os.environ.get("VOICEPI_DICTIONARY")
        try:
            os.environ.pop("VOICEPI_DICTIONARY", None)
            self.assertEqual(
                store.resolve_dictionary_path(),
                store.default_dictionary_path(),
            )
        finally:
            if old is not None:
                os.environ["VOICEPI_DICTIONARY"] = old


class SanitizeDictionaryPathTests(unittest.TestCase):
    def test_resolves_and_keeps_json_suffix(self):
        d = tempfile.mkdtemp()
        p = store.sanitize_dictionary_path(Path(d) / "sub" / "dictionary.json")
        self.assertTrue(p.is_absolute())
        self.assertEqual(p.suffix, ".json")

    def test_expands_user_home(self):
        p = store.sanitize_dictionary_path("~/whisper-dictate-dict.json")
        self.assertNotIn("~", str(p))
        self.assertTrue(p.is_absolute())

    def test_rejects_non_json_suffix(self):
        with self.assertRaises(ValueError):
            store.sanitize_dictionary_path("/tmp/.bashrc")

    def test_rejects_parent_traversal(self):
        with self.assertRaises(ValueError):
            store.sanitize_dictionary_path("../../etc/evil.json")

    def test_rejects_blank(self):
        with self.assertRaises(ValueError):
            store.sanitize_dictionary_path("   ")

    def test_write_rejects_non_json_path(self):
        d = tempfile.mkdtemp()
        with self.assertRaises(ValueError):
            store.write_terms(Path(d) / "dictionary.txt", ["X"])


class LoadAndWriteTests(unittest.TestCase):
    def _tmp(self) -> Path:
        d = tempfile.mkdtemp()
        return Path(d) / "dictionary.json"

    def test_missing_file_yields_empty_doc_and_no_terms(self):
        p = self._tmp()
        self.assertEqual(store.load_dictionary_document(p), {})
        self.assertEqual(store.load_terms(p), [])

    def test_loads_string_and_object_terms(self):
        p = self._tmp()
        p.write_text(json.dumps({
            "terms": ["Slack", {"term": "Claude Code"}, "", "  "],
            "replacements": {"Cloud Code": "Claude Code"},
        }), encoding="utf-8")
        self.assertEqual(store.load_terms(p), ["Slack", "Claude Code"])

    def test_invalid_json_raises_valueerror(self):
        p = self._tmp()
        p.write_text("{not json", encoding="utf-8")
        with self.assertRaises(ValueError):
            store.load_dictionary_document(p)

    def test_non_object_root_raises_valueerror(self):
        p = self._tmp()
        p.write_text("[1,2,3]", encoding="utf-8")
        with self.assertRaises(ValueError):
            store.load_dictionary_document(p)

    def test_write_preserves_other_keys(self):
        p = self._tmp()
        base = {
            "terms": ["Old"],
            "replacements": {"Cloud Code": "Claude Code"},
            "notes": "keep me",
        }
        p.write_text(json.dumps(base), encoding="utf-8")
        document = store.load_dictionary_document(p)
        store.write_terms(p, ["Old", "New"], base=document)
        reloaded = json.loads(p.read_text(encoding="utf-8"))
        self.assertEqual(reloaded["terms"], ["Old", "New"])
        self.assertEqual(reloaded["replacements"], {"Cloud Code": "Claude Code"})
        self.assertEqual(reloaded["notes"], "keep me")

    def test_write_creates_parent_dirs_and_default_replacements(self):
        d = tempfile.mkdtemp()
        p = Path(d) / "nested" / "dir" / "dictionary.json"
        store.write_terms(p, ["A"])
        self.assertTrue(p.exists())
        reloaded = json.loads(p.read_text(encoding="utf-8"))
        self.assertEqual(reloaded["terms"], ["A"])
        self.assertEqual(reloaded["replacements"], {})

    def test_written_file_round_trips_through_load_terms(self):
        p = self._tmp()
        store.write_terms(p, ["Kubernetes", "Parakeet"])
        self.assertEqual(store.load_terms(p), ["Kubernetes", "Parakeet"])
