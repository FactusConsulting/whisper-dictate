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

class BenchmarkTests(unittest.TestCase):
    def test_corpus_manifest_loads_and_scores_terms(self):
        from whisper_dictate import vp_benchmark

        item = vp_benchmark.load_corpus("benchmark/corpus.json")[0]

        self.assertEqual(item.id, "da-short-001")
        self.assertTrue(str(item.audio).endswith("benchmark\\audio\\da-short-001.wav") or
                        str(item.audio).endswith("benchmark/audio/da-short-001.wav"))
        self.assertEqual(vp_benchmark.wer("Claude Code virker", "Claude virker"), 1 / 3)
        report = vp_benchmark.term_report(["Claude Code", "Codex"], "Claude Code works")
        self.assertEqual(report["hits"], ["Claude Code"])
        self.assertEqual(report["misses"], ["Codex"])

    def test_corpus_annotates_benchmark_event(self):
        from whisper_dictate import vp_benchmark

        item = vp_benchmark.CorpusItem(
            id="x",
            text="Claude Code and Codex",
            audio=Path("x.wav"),
            language="en",
            category="tech",
            terms=("Claude Code", "Codex"),
        )
        event = vp_benchmark.annotate_event({"text": "Claude Code and codec"}, item)

        self.assertEqual(event["corpus_id"], "x")
        self.assertEqual(event["corpus_language"], "en")
        self.assertGreater(event["wer"], 0)
        self.assertEqual(event["term_hits"], ["Claude Code"])
        self.assertEqual(event["term_misses"], ["Codex"])

    def test_parse_backend_specs_supports_models(self):
        from whisper_dictate import vp_benchmark

        specs = vp_benchmark.parse_backend_specs(
            "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3,openai:gpt-4o-mini-transcribe")

        self.assertEqual(specs[0].backend, "whisper")
        self.assertEqual(specs[0].model, "large-v3")
        self.assertEqual(specs[1].backend, "parakeet")
        self.assertEqual(specs[1].model, "nvidia/parakeet-tdt-0.6b-v3")
        self.assertEqual(specs[2].backend, "openai")
        self.assertEqual(specs[2].model, "gpt-4o-mini-transcribe")

    def test_parse_backend_specs_rejects_unknown_backend(self):
        from whisper_dictate import vp_benchmark

        with self.assertRaisesRegex(ValueError, "unsupported benchmark backend"):
            vp_benchmark.parse_backend_specs("cloud:gpt-4o-transcribe")

    def test_benchmark_run_one_invokes_transcribe_file_json(self):
        from whisper_dictate import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=0,
            stdout='{"event":"file_transcription","text":"hello"}\n',
            stderr="",
        )
        with patch("whisper_dictate.vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="whisper:large-v3", backend="whisper", model="large-v3"),
                python_exe="python",
                base_env={},
            )

        cmd = run.call_args.args[0]
        env = run.call_args.kwargs["env"]
        self.assertEqual(cmd, [
            "python", "-m", "whisper_dictate.runtime", "--transcribe-file", "sample.wav", "--json"
        ])
        self.assertIn("src", env["PYTHONPATH"])
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "whisper")
        self.assertEqual(env["VOICEPI_MODEL"], "large-v3")
        self.assertTrue(event["benchmark_success"])
        self.assertEqual(event["benchmark_backend_spec"], "whisper:large-v3")

    def test_benchmark_parakeet_model_uses_parakeet_env(self):
        from whisper_dictate import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=1,
            stdout="",
            stderr="missing nemo",
        )
        with patch("whisper_dictate.vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="parakeet:nvidia/model", backend="parakeet",
                    model="nvidia/model"),
                python_exe="python",
                base_env={},
            )

        env = run.call_args.kwargs["env"]
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "parakeet")
        self.assertEqual(env["VOICEPI_PARAKEET_MODEL"], "nvidia/model")
        self.assertFalse(event["benchmark_success"])
        self.assertIn("missing nemo", event["benchmark_error"])

    def test_benchmark_jsonl_writes_one_line_per_file_backend(self):
        from whisper_dictate import vp_benchmark

        events = []

        def fake_run_one(audio_file, spec):
            event = {
                "event": "benchmark_result",
                "source_file": str(audio_file),
                "benchmark_backend_spec": spec.raw,
                "text": "ok",
            }
            events.append(event)
            return event

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            with patch("whisper_dictate.vp_benchmark.run_one", side_effect=fake_run_one):
                results = vp_benchmark.run_benchmark(
                    ["a.wav", "b.wav"], "whisper,parakeet", output_jsonl=path)
            with open(path, encoding="utf-8") as f:
                lines = [json.loads(line) for line in f]
        finally:
            os.remove(path)

        self.assertEqual(len(results), 4)
        self.assertEqual(len(lines), 4)
        self.assertEqual(lines[0]["benchmark_backend_spec"], "whisper")

    def test_benchmark_corpus_writes_skipped_rows_for_missing_audio(self):
        from whisper_dictate import vp_benchmark

        manifest = {
            "items": [{
                "id": "sample",
                "language": "en",
                "category": "unit",
                "text": "Hello Codex",
                "audio": "missing.wav",
                "terms": ["Codex"],
            }]
        }
        with tempfile.TemporaryDirectory() as d:
            manifest_path = os.path.join(d, "corpus.json")
            output_path = os.path.join(d, "out.jsonl")
            with open(manifest_path, "w", encoding="utf-8") as f:
                json.dump(manifest, f)

            results = vp_benchmark.run_benchmark(
                None,
                "whisper",
                output_jsonl=output_path,
                corpus_manifest=manifest_path,
            )
            with open(output_path, encoding="utf-8") as f:
                rows = [json.loads(line) for line in f]

        self.assertEqual(len(results), 1)
        self.assertEqual(rows[0]["corpus_id"], "sample")
        self.assertTrue(rows[0]["benchmark_skipped"])
        self.assertEqual(rows[0]["benchmark_backend_spec"], "whisper")
        self.assertEqual(rows[0]["benchmark_backend"], "whisper")
        self.assertIsNone(rows[0]["benchmark_model"])
        self.assertEqual(rows[0]["term_misses"], ["Codex"])

    def test_benchmark_corpus_reuses_loaded_model_per_backend(self):
        from whisper_dictate import vp_benchmark

        manifest = {
            "audio_dir": "audio",
            "items": [
                {"id": "one", "language": "da", "text": "Hej Codex", "terms": ["Codex"]},
                {"id": "two", "language": "en", "text": "Hello Claude Code", "terms": ["Claude Code"]},
            ]
        }
        calls = []

        def fake_transcribe(model, path, lang, **kwargs):
            calls.append((model, Path(path).name, lang, kwargs["stt_backend"]))
            text = "Hej Codex" if lang == "da" else "Hello Claude Code"
            return {"event": "file_transcription", "text": text, "source_file": str(path)}

        with tempfile.TemporaryDirectory() as d:
            manifest_path = Path(d) / "corpus.json"
            audio_dir = Path(d) / "audio"
            audio_dir.mkdir()
            for name in ("one.wav", "two.wav"):
                (audio_dir / name).write_bytes(b"not used by patched transcriber")
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            try:
                with patch("whisper_dictate.vp_benchmark._load_model_for_spec", return_value=("model", "m", "cpu", "int8")) as load:
                    with patch("whisper_dictate.runtime.transcribe_file_event", side_effect=fake_transcribe):
                        results = vp_benchmark.run_benchmark(
                            None,
                            "whisper:tiny",
                            corpus_manifest=manifest_path,
                        )
            finally:
                sys.modules.pop("whisper_dictate.runtime", None)
                sys.modules.pop("vp_transcribe", None)

        self.assertEqual(load.call_count, 1)
        self.assertEqual(len(calls), 2)
        self.assertEqual([c[2] for c in calls], ["da", "en"])
        self.assertTrue(all(r["benchmark_success"] for r in results))
        self.assertEqual(results[1]["term_hits"], ["Claude Code"])

    def test_record_corpus_imports_sounddevice_lazily_with_help(self):
        with open("scripts/benchmark/record-corpus.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def load_sounddevice", script)
        self.assertIn("Missing recorder dependency: sounddevice", script)
        self.assertIn("py -3.12 -m pip install", script)
        self.assertIn("sounddevice>=0.4,<0.6", script)
        self.assertIn("sd = load_sounddevice()", script)
        self.assertNotIn("\nimport sounddevice as sd\n", script)

    def test_parser_accepts_benchmark_options(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--benchmark-files", "a.wav", "b.wav",
            "--benchmark-corpus", "benchmark/corpus.json",
            "--benchmark-backends", "whisper,parakeet",
            "--benchmark-jsonl", "out.jsonl",
        ])

        self.assertEqual(args.benchmark_files, ["a.wav", "b.wav"])
        self.assertEqual(args.benchmark_corpus, "benchmark/corpus.json")
        self.assertEqual(args.benchmark_backends, "whisper,parakeet")
        self.assertEqual(args.benchmark_jsonl, "out.jsonl")

    def test_parser_exposes_run_benchmark_flag(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        self.assertTrue(
            vp_cli.build_arg_parser().parse_args(["--run-benchmark"]).run_benchmark)
        # Defaults off so a normal dictation run is unaffected.
        self.assertFalse(vp_cli.build_arg_parser().parse_args([]).run_benchmark)

    def test_summarize_results_counts_and_averages_scored_items(self):
        from whisper_dictate import vp_benchmark

        results = [
            {"benchmark_success": True, "wer": 0.0, "cer": 0.0},
            {"benchmark_success": True, "wer": 0.2, "cer": 0.1},
            # A skipped row (missing audio) carries wer but must not be scored.
            {"benchmark_skipped": True, "benchmark_success": False, "wer": 1.0},
            # A hard failure with no wer field.
            {"benchmark_success": False},
        ]
        summary = vp_benchmark.summarize_results(results)

        self.assertEqual(summary["total"], 4)
        self.assertEqual(summary["passed"], 2)
        self.assertEqual(summary["skipped"], 1)
        self.assertEqual(summary["failed"], 1)
        self.assertEqual(summary["scored"], 2)
        self.assertAlmostEqual(summary["avg_wer"], 0.1)
        self.assertAlmostEqual(summary["avg_cer"], 0.05)

    def test_summarize_results_handles_all_skipped(self):
        from whisper_dictate import vp_benchmark

        summary = vp_benchmark.summarize_results([
            {"benchmark_skipped": True, "benchmark_success": False, "wer": 1.0},
        ])
        # No scored items → averages are None (avoids a divide-by-zero).
        self.assertEqual(summary["scored"], 0)
        self.assertIsNone(summary["avg_wer"])
        self.assertIsNone(summary["avg_cer"])

    def test_summarize_results_averages_cer_over_cer_bearing_rows_only(self):
        from whisper_dictate import vp_benchmark

        # Two scored (WER-bearing) rows, but only one carries a `cer` field.
        # avg_cer must divide by the count of CER-bearing rows (1), not by the
        # number of scored rows (2) — otherwise the average is understated.
        results = [
            {"benchmark_success": True, "wer": 0.2, "cer": 0.4},
            {"benchmark_success": True, "wer": 0.4},
        ]
        summary = vp_benchmark.summarize_results(results)

        self.assertEqual(summary["scored"], 2)
        self.assertAlmostEqual(summary["avg_wer"], 0.3)
        # 0.4 / 1, NOT 0.4 / 2 == 0.2.
        self.assertAlmostEqual(summary["avg_cer"], 0.4)

    def test_summarize_results_cer_none_when_no_row_has_cer(self):
        from whisper_dictate import vp_benchmark

        # Scored rows exist (WER present) but none report CER → avg_cer is None.
        summary = vp_benchmark.summarize_results([
            {"benchmark_success": True, "wer": 0.2},
            {"benchmark_success": True, "wer": 0.4},
        ])
        self.assertEqual(summary["scored"], 2)
        self.assertAlmostEqual(summary["avg_wer"], 0.3)
        self.assertIsNone(summary["avg_cer"])

    def test_format_summary_line_renders_concise_benchmark_line(self):
        from whisper_dictate import vp_benchmark

        line = vp_benchmark.format_summary_line({
            "total": 4, "passed": 2, "failed": 1, "skipped": 1,
            "scored": 2, "avg_wer": 0.1, "avg_cer": 0.05,
        })
        self.assertTrue(line.startswith("[benchmark] "))
        self.assertIn("2/4 passed", line)
        self.assertIn("1 skipped", line)
        self.assertIn("1 failed", line)
        self.assertIn("avg WER 10.0%", line)

    def test_run_corpus_benchmark_defaults_manifest_and_prints_summary(self):
        from whisper_dictate import vp_benchmark

        captured = {}

        def fake_run_benchmark(audio_files, backend_specs, *, output_jsonl=None,
                               corpus_manifest=None):
            captured["audio_files"] = audio_files
            captured["backend_specs"] = backend_specs
            captured["corpus_manifest"] = corpus_manifest
            captured["output_jsonl"] = output_jsonl
            return [{"benchmark_success": True, "wer": 0.0, "cer": 0.0}]

        with patch("whisper_dictate.vp_benchmark.run_benchmark",
                   side_effect=fake_run_benchmark):
            with _capture_stdout() as out:
                summary = vp_benchmark.run_corpus_benchmark()

        # No manifest passed → defaults to the golden corpus, no audio_files.
        self.assertIsNone(captured["audio_files"])
        self.assertEqual(captured["corpus_manifest"],
                         vp_benchmark.DEFAULT_CORPUS_MANIFEST)
        self.assertEqual(summary["passed"], 1)
        # The concise [benchmark] line lands on stdout for the UI log.
        self.assertIn("[benchmark] 1/1 passed", out.getvalue())

    def test_handle_benchmark_routes_run_benchmark_to_corpus_runner(self):
        from whisper_dictate import runtime

        ap = types.SimpleNamespace(error=lambda msg: (_ for _ in ()).throw(
            AssertionError(f"ap.error called: {msg}")))
        args = types.SimpleNamespace(
            run_benchmark=True,
            benchmark_files=None,
            benchmark_corpus=None,
            benchmark_backends="whisper",
            benchmark_jsonl=None,
        )
        with patch("whisper_dictate.vp_benchmark.run_corpus_benchmark") as corpus:
            with patch("whisper_dictate.vp_benchmark.run_benchmark") as plain:
                runtime._handle_benchmark(args, ap)

        # --run-benchmark dispatches to the summarizing corpus runner, NOT the
        # raw per-file run_benchmark.
        corpus.assert_called_once()
        plain.assert_not_called()
        self.assertEqual(corpus.call_args.args[0], None)  # default manifest
        self.assertEqual(corpus.call_args.args[1], "whisper")

class HistoryTests(unittest.TestCase):
    def test_read_history_keeps_core_fields_written_by_rust(self):
        from whisper_dictate import runtime

        event = {
            "ts": 1,
            "event": "utterance",
            "text": "hello",
            "raw_text": "hallo",
            "stt_backend": "whisper",
            "target_title": "Editor",
            "large_unused_blob": "drop",
        }
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            os.remove(path)
            with open(path, "w", encoding="utf-8") as f:
                f.write(json.dumps(runtime._history_event(event), ensure_ascii=False) + "\n")
            rows = runtime.read_history(10, Path(path))
        finally:
            try:
                os.remove(path)
            except OSError:
                pass

        self.assertEqual(rows[0]["text"], "hello")
        self.assertEqual(rows[0]["raw_text"], "hallo")
        self.assertEqual(rows[0]["target_title"], "Editor")
        self.assertNotIn("large_unused_blob", rows[0])

    def test_history_last_returns_latest_item(self):
        from whisper_dictate import runtime

        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "first"}) + "\n")
            f.write(json.dumps({"text": "second"}) + "\n")
        try:
            item = runtime.last_history(Path(path))
        finally:
            os.remove(path)

        self.assertEqual(item["text"], "second")

    def test_history_can_be_disabled(self):
        old = os.environ.get("VOICEPI_HISTORY_ENABLED")
        os.environ["VOICEPI_HISTORY_ENABLED"] = "0"
        from whisper_dictate import runtime

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            os.remove(path)
            written = runtime.append_history({"text": "hidden"}, Path(path))
            self.assertIsNone(written)
            self.assertFalse(os.path.exists(path))
        finally:
            if old is None:
                os.environ.pop("VOICEPI_HISTORY_ENABLED", None)
            else:
                os.environ["VOICEPI_HISTORY_ENABLED"] = old

    def test_history_copy_last_uses_clipboard(self):
        from whisper_dictate import runtime

        copied = {}

        fake_pyperclip = types.ModuleType("pyperclip")
        fake_pyperclip.copy = lambda text: copied.setdefault("text", text)
        sys.modules["pyperclip"] = fake_pyperclip
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "copy me"}) + "\n")
        try:
            text = runtime.copy_last_to_clipboard(Path(path))
        finally:
            os.remove(path)
            sys.modules.pop("pyperclip", None)

        self.assertEqual(text, "copy me")
        self.assertEqual(copied["text"], "copy me")

    def test_parser_accepts_history_options(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args(["--history-list"])
        self.assertEqual(args.history_list, 10)
        args = vp_cli.build_arg_parser().parse_args(["--history-list", "3"])
        self.assertEqual(args.history_list, 3)
        args = vp_cli.build_arg_parser().parse_args(["--history-last"])
        self.assertTrue(args.history_last)
        args = vp_cli.build_arg_parser().parse_args(["--history-copy-last"])
        self.assertTrue(args.history_copy_last)
        args = vp_cli.build_arg_parser().parse_args(["--history-reinject-last"])
        self.assertTrue(args.history_reinject_last)

    def test_runtime_appends_history_after_metrics(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('_append_jsonl(self.metrics_jsonl, event)', script)
        self.assertIn("_append_history(event)", script)
        self.assertLess(script.index("_append_jsonl(self.metrics_jsonl, event)"),
                        script.index("_append_history(event)"))

class ProfileTests(unittest.TestCase):
    def test_runtime_records_active_profile_in_metrics(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def _profiled_config", script)
        self.assertIn("_apply_profile_settings", script)
        self.assertIn("[profile] active:", script)
        self.assertIn('"profile": getattr(self, "_active_profile_name", None)', script)

    def test_history_keeps_profile_field(self):
        from whisper_dictate import runtime

        event = {"text": "hello", "profile": "Claude terminal"}
        stored = runtime._history_event(event)

        self.assertEqual(stored["profile"], "Claude terminal")
