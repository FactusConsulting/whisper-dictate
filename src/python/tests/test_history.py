"""Unit tests for vp_history (extracted from runtime.py).

Local dictation history: JSONL read, event filtering, enable/path resolution,
clipboard copy and append — all with the Rust helper / clipboard stubbed and a
temp JSONL so no real user state is touched.
"""
from helpers import (
    Path,
    _env,
    json,
    patch,
    sys,
    tempfile,
    types,
    unittest,
)

from whisper_dictate import vp_history


def _write_jsonl(path: Path, rows):
    path.write_text(
        "\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n",
        encoding="utf-8",
    )


class HistoryReadTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.path = Path(self.dir.name) / "history.jsonl"

    def test_read_history_returns_last_n_in_order(self):
        _write_jsonl(self.path, [{"text": f"t{i}"} for i in range(5)])
        rows = vp_history.read_history(2, self.path)
        self.assertEqual([r["text"] for r in rows], ["t3", "t4"])

    def test_read_history_missing_file_is_empty(self):
        self.assertEqual(vp_history.read_history(10, self.path), [])

    def test_read_history_skips_blank_and_invalid_lines(self):
        self.path.write_text(
            '{"text": "ok"}\n\nnot-json\n{"text": "ok2"}\n', encoding="utf-8"
        )
        rows = vp_history.read_history(10, self.path)
        self.assertEqual([r["text"] for r in rows], ["ok", "ok2"])

    def test_read_history_clamps_nonpositive_limit(self):
        # limit <= 0 must not dump the whole file (matches the Rust clamp >= 1).
        _write_jsonl(self.path, [{"text": f"t{i}"} for i in range(5)])
        self.assertEqual(
            [r["text"] for r in vp_history.read_history(0, self.path)], ["t4"]
        )
        self.assertEqual(
            [r["text"] for r in vp_history.read_history(-3, self.path)], ["t4"]
        )

    def test_last_history(self):
        _write_jsonl(self.path, [{"text": "a"}, {"text": "b"}])
        self.assertEqual(vp_history.last_history(self.path)["text"], "b")
        self.assertIsNone(vp_history.last_history(Path(self.dir.name) / "none.jsonl"))


class HistorySettingsTests(unittest.TestCase):
    def test_history_enabled_default_and_override(self):
        with _env(VOICEPI_HISTORY_ENABLED=None):
            self.assertTrue(vp_history.history_enabled())
        with _env(VOICEPI_HISTORY_ENABLED="0"):
            self.assertFalse(vp_history.history_enabled())

    def test_history_path_override(self):
        with _env(VOICEPI_HISTORY_JSONL="/tmp/custom-hist.jsonl"):
            self.assertEqual(
                vp_history.history_path(), Path("/tmp/custom-hist.jsonl").expanduser()
            )

    def test_history_event_filters_to_known_keys(self):
        event = {"text": "hi", "secret": "x", "model": "m", "event": "utterance"}
        filtered = vp_history._history_event(event)
        self.assertIn("text", filtered)
        self.assertIn("model", filtered)
        self.assertNotIn("secret", filtered)


class HistoryWriteTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.path = Path(self.dir.name) / "history.jsonl"

    def test_append_history_calls_rust_helper_when_enabled(self):
        with _env(VOICEPI_HISTORY_ENABLED="1"), \
                patch.object(vp_history, "_rust_json") as rust:
            out = vp_history.append_history({"text": "hi"}, self.path)
        self.assertEqual(out, self.path)
        rust.assert_called_once()
        self.assertEqual(rust.call_args.args[0], "append-history")

    def test_append_history_disabled_is_noop(self):
        with _env(VOICEPI_HISTORY_ENABLED="0"), \
                patch.object(vp_history, "_rust_json") as rust:
            out = vp_history.append_history({"text": "hi"}, self.path)
        self.assertIsNone(out)
        rust.assert_not_called()

    def test_copy_last_to_clipboard(self):
        _write_jsonl(self.path, [{"text": "copy me"}])
        fake = types.SimpleNamespace(copied=None)
        fake_mod = types.ModuleType("pyperclip")
        fake_mod.copy = lambda t: setattr(fake, "copied", t)
        with patch.dict(sys.modules, {"pyperclip": fake_mod}):
            text = vp_history.copy_last_to_clipboard(self.path)
        self.assertEqual(text, "copy me")
        self.assertEqual(fake.copied, "copy me")

    def test_copy_last_to_clipboard_empty_raises(self):
        with self.assertRaises(RuntimeError):
            vp_history.copy_last_to_clipboard(self.path)


if __name__ == "__main__":
    unittest.main()
