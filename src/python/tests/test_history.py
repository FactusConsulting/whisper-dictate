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

    def test_read_history_keeps_requested_tail_for_large_files(self):
        _write_jsonl(self.path, [{"text": f"t{i}"} for i in range(5000)])
        rows = vp_history.read_history(3, self.path)
        self.assertEqual([r["text"] for r in rows], ["t4997", "t4998", "t4999"])

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

    def test_append_record_sinks_uses_combined_helper_when_available(self):
        calls = []

        def fake_rust_json(command, payload, *args, **kwargs):
            calls.append((command, payload, args))
            return {"ok": True} if command == "append-record-sinks" else None

        with patch.object(vp_history, "_rust_json", fake_rust_json), \
                patch.object(vp_history, "history_enabled", return_value=True), \
                patch.object(vp_history, "history_path", return_value=self.path):
            vp_history.append_record_sinks(
                {"event": "utterance", "text": "hi"},
                metrics_jsonl=str(Path(self.dir.name) / "metrics.jsonl"),
                json_output=True,
            )

        self.assertEqual([call[0] for call in calls], ["append-record-sinks"])

    def test_append_record_sinks_falls_back_to_legacy_helpers(self):
        calls = []

        def fake_rust_json(command, payload, *args, **kwargs):
            calls.append((command, args))
            return None

        with patch.object(vp_history, "_rust_json", fake_rust_json), \
                patch.object(vp_history, "history_enabled", return_value=True), \
                patch.object(vp_history, "history_path", return_value=self.path):
            vp_history.append_record_sinks(
                {"event": "utterance", "text": "hi"},
                metrics_jsonl=str(Path(self.dir.name) / "metrics.jsonl"),
                json_output=True,
            )

        self.assertEqual(
            [call[0] for call in calls],
            ["append-record-sinks", "append-jsonl", "append-history"],
        )

    def test_append_record_sinks_ignores_whitespace_metrics_path(self):
        calls = []

        def fake_rust_json(command, payload, *args, **kwargs):
            calls.append((command, payload, args))
            return None

        with patch.object(vp_history, "_rust_json", fake_rust_json), \
                patch.object(vp_history, "history_enabled", return_value=False):
            vp_history.append_record_sinks(
                {"event": "utterance", "text": "hi"},
                metrics_jsonl="   ",
                json_output=True,
            )

        self.assertEqual([], calls)

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


class HistoryCommandHelperTests(unittest.TestCase):
    """_history_list / _history_last extracted from run_history_command."""

    def test_history_list_json_uses_read_history(self):
        rows = [{"text": "hello", "ts": "t", "stt_backend": "whisper"}]
        with patch.object(vp_history, "read_history", return_value=rows) as rh, \
                patch("builtins.print") as pr:
            vp_history._history_list(5, as_json=True)
        rh.assert_called_once_with(5)
        self.assertIn("hello", pr.call_args[0][0])

    def test_history_list_text_falls_back_when_rust_helper_absent(self):
        rows = [{"text": "hi", "ts": "2026", "stt_backend": "whisper"}]
        with patch.object(vp_history, "_run_rust_history_command", return_value=False), \
                patch.object(vp_history, "read_history", return_value=rows), \
                patch("builtins.print") as pr:
            vp_history._history_list(3, as_json=False)
        printed = " ".join(str(c.args[0]) for c in pr.call_args_list)
        self.assertIn("hi", printed)

    def test_history_last_json_and_text_fallback(self):
        with patch.object(vp_history, "last_history", return_value={"text": "last one"}), \
                patch("builtins.print") as pr:
            vp_history._history_last(as_json=True)
        self.assertIn("last one", pr.call_args[0][0])
        with patch.object(vp_history, "_run_rust_history_command", return_value=False), \
                patch.object(vp_history, "last_history", return_value={"text": "last one"}), \
                patch("builtins.print") as pr2:
            vp_history._history_last(as_json=False)
        self.assertIn("last one", pr2.call_args[0][0])

    def test_run_history_command_dispatches_each_action(self):
        with patch.object(vp_history, "_history_list") as hl, \
                patch.object(vp_history, "_history_last") as hla, \
                patch.object(vp_history, "copy_last_to_clipboard", return_value="x") as cl, \
                patch.object(vp_history, "reinject_last", return_value="y") as rj, \
                patch("builtins.print"):
            vp_history.run_history_command("list", limit=7, as_json=True)
            hl.assert_called_once_with(7, True)
            vp_history.run_history_command("last", as_json=False)
            hla.assert_called_once_with(False)
            vp_history.run_history_command("copy-last")
            cl.assert_called_once()
            vp_history.run_history_command("reinject-last")
            rj.assert_called_once()

    def test_run_history_command_raises_and_logs_on_unknown_action(self):
        with patch("builtins.print"), self.assertRaises(RuntimeError):
            vp_history.run_history_command("bogus")


if __name__ == "__main__":
    unittest.main()
