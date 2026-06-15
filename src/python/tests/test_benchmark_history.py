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

    def _write_profile_corpus(self) -> Path:
        d = Path(tempfile.mkdtemp())
        manifest = d / "corpus.json"
        manifest.write_text(json.dumps({
            "version": 1,
            "audio_dir": "audio",
            "items": [
                {"id": "da-tech", "language": "da", "category": "mixed_technical",
                 "text": "da tech", "terms": []},
                {"id": "en-short", "language": "en", "category": "short_english",
                 "text": "en short", "terms": []},
            ],
        }), encoding="utf-8")
        return manifest

    def test_benchmark_profile_filters_corpus_subset(self):
        from whisper_dictate import vp_benchmark
        from whisper_dictate.vp_corpus_profile import build_profile

        manifest = self._write_profile_corpus()
        # No audio files exist next to this manifest, so every selected item is a
        # skip event and NO model is loaded — the profile filtering is what we test.
        results = vp_benchmark.run_benchmark(
            None,
            "whisper",
            corpus_manifest=manifest,
            profile=build_profile(language="da"),
        )
        ids = {r["corpus_id"] for r in results}
        self.assertEqual(ids, {"da-tech"})

    def test_benchmark_empty_profile_runs_all(self):
        from whisper_dictate import vp_benchmark
        from whisper_dictate.vp_corpus_profile import build_profile

        manifest = self._write_profile_corpus()
        results = vp_benchmark.run_benchmark(
            None, "whisper", corpus_manifest=manifest, profile=build_profile())
        self.assertEqual({r["corpus_id"] for r in results}, {"da-tech", "en-short"})

    def test_benchmark_profile_no_match_raises(self):
        from whisper_dictate import vp_benchmark
        from whisper_dictate.vp_corpus_profile import build_profile

        manifest = self._write_profile_corpus()
        with self.assertRaisesRegex(ValueError, "matched no items"):
            vp_benchmark.run_benchmark(
                None, "whisper", corpus_manifest=manifest,
                profile=build_profile(language="fr"))

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
                               corpus_manifest=None, appdata=None, profile=None):
            captured["audio_files"] = audio_files
            captured["backend_specs"] = backend_specs
            captured["corpus_manifest"] = corpus_manifest
            captured["output_jsonl"] = output_jsonl
            captured["appdata"] = appdata
            captured["profile"] = profile
            return [{"benchmark_success": True, "wer": 0.0, "cer": 0.0}]

        with patch("whisper_dictate.vp_benchmark.run_benchmark",
                   side_effect=fake_run_benchmark):
            with _capture_stdout() as out:
                summary = vp_benchmark.run_corpus_benchmark()

        # No manifest/app-root passed → resolves the dev-checkout golden corpus
        # under the CWD ("."), no audio_files. Run from the repo root, that path
        # exists, so the resolved manifest ends with benchmark/corpus.json.
        self.assertIsNone(captured["audio_files"])
        self.assertTrue(
            str(captured["corpus_manifest"]).replace("\\", "/").endswith("benchmark/corpus.json"))
        self.assertEqual(summary["passed"], 1)
        # The concise [benchmark] line lands on stdout for the UI log.
        self.assertIn("[benchmark] 1/1 passed", out.getvalue())

    def test_resolve_corpus_manifest_priority_order(self):
        import tempfile
        from whisper_dictate import vp_benchmark

        # (a) explicit always wins, returned verbatim even if it does not exist.
        self.assertEqual(
            vp_benchmark.resolve_corpus_manifest("approot", "given/corpus.json", "appd"),
            Path("given/corpus.json"))

        with tempfile.TemporaryDirectory() as d:
            app_root = Path(d) / "app"
            appdata = Path(d) / "appdata"
            app_manifest = app_root / "benchmark" / "corpus.json"
            appdata_manifest = appdata / "benchmark" / "corpus.json"

            # (d) nothing exists anywhere → None.
            self.assertIsNone(
                vp_benchmark.resolve_corpus_manifest(app_root, None, appdata))

            # (c) only the per-user appdata manifest exists.
            appdata_manifest.parent.mkdir(parents=True)
            appdata_manifest.write_text("{}", encoding="utf-8")
            self.assertEqual(
                vp_benchmark.resolve_corpus_manifest(app_root, None, appdata),
                appdata_manifest)

            # (b) the shipped/dev app-root manifest takes precedence over appdata.
            app_manifest.parent.mkdir(parents=True)
            app_manifest.write_text("{}", encoding="utf-8")
            self.assertEqual(
                vp_benchmark.resolve_corpus_manifest(app_root, None, appdata),
                app_manifest)

    def test_resolve_item_audio_falls_back_to_appdata(self):
        import tempfile
        from whisper_dictate import vp_benchmark

        with tempfile.TemporaryDirectory() as d:
            appdata = Path(d) / "appdata"
            shipped = Path(d) / "benchmark" / "audio" / "item.wav"
            fallback = appdata / "benchmark" / "audio" / "item.wav"

            # Shipped/manifest-relative recording present → used as-is.
            shipped.parent.mkdir(parents=True)
            shipped.write_bytes(b"x")
            self.assertEqual(
                vp_benchmark.resolve_item_audio(shipped, appdata), shipped)

            # Missing in place but present in the per-user appdata dir → fallback.
            missing = Path(d) / "benchmark" / "audio" / "item2.wav"
            fallback2 = appdata / "benchmark" / "audio" / "item2.wav"
            fallback2.parent.mkdir(parents=True)
            fallback2.write_bytes(b"x")
            self.assertEqual(
                vp_benchmark.resolve_item_audio(missing, appdata), fallback2)

            # Missing everywhere → original path returned (caller records a skip).
            gone = Path(d) / "benchmark" / "audio" / "gone.wav"
            self.assertEqual(
                vp_benchmark.resolve_item_audio(gone, appdata), gone)
            # No appdata dir given → original path, no fallback attempted.
            self.assertEqual(
                vp_benchmark.resolve_item_audio(gone, None), gone)

    def test_run_corpus_benchmark_no_corpus_prints_clear_line_and_returns_none(self):
        import tempfile
        from whisper_dictate import vp_benchmark

        with tempfile.TemporaryDirectory() as d:
            empty_root = Path(d) / "app"
            empty_root.mkdir()
            with patch("whisper_dictate.vp_benchmark.appdata_dir",
                       return_value=Path(d) / "appdata"):
                with patch("whisper_dictate.vp_benchmark.run_benchmark") as ran:
                    with _capture_stdout() as out:
                        result = vp_benchmark.run_corpus_benchmark(app_root=empty_root)

        # No corpus anywhere → a single clear line, exit-0 outcome, no model run.
        self.assertIsNone(result)
        ran.assert_not_called()
        line = out.getvalue()
        self.assertIn("[benchmark] no corpus manifest found", line)
        self.assertIn("benchmark", line)
        self.assertIn("docs", line)

    def test_run_benchmark_uses_appdata_audio_fallback_for_missing_recording(self):
        import tempfile
        from whisper_dictate import vp_benchmark

        manifest = {
            "audio_dir": "audio",
            "items": [
                {"id": "one", "language": "da", "text": "Hej Codex", "terms": []},
            ],
        }

        def fake_transcribe(model, path, lang, **kwargs):
            return {"event": "file_transcription", "text": "Hej Codex",
                    "source_file": str(path)}

        with tempfile.TemporaryDirectory() as d:
            manifest_path = Path(d) / "corpus.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            # No audio next to the manifest; put the recording in the appdata dir.
            appdata = Path(d) / "appdata"
            fallback = appdata / "benchmark" / "audio" / "one.wav"
            fallback.parent.mkdir(parents=True)
            fallback.write_bytes(b"not used by patched transcriber")

            try:
                with patch("whisper_dictate.vp_benchmark._load_model_for_spec",
                           return_value=("model", "m", "cpu", "int8")):
                    with patch("whisper_dictate.runtime.transcribe_file_event",
                               side_effect=fake_transcribe):
                        results = vp_benchmark.run_benchmark(
                            None, "whisper:tiny",
                            corpus_manifest=manifest_path, appdata=appdata)
            finally:
                sys.modules.pop("whisper_dictate.runtime", None)
                sys.modules.pop("vp_transcribe", None)

        # The item scored (not skipped) because the audio resolved via appdata.
        self.assertEqual(len(results), 1)
        self.assertTrue(results[0]["benchmark_success"])
        self.assertFalse(results[0].get("benchmark_skipped"))

    def test_format_summary_line_hints_audio_when_all_skipped_for_missing_audio(self):
        from whisper_dictate import vp_benchmark

        summary = vp_benchmark.summarize_results([
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="a", text="x", audio=Path("a.wav")),
                vp_benchmark.MISSING_AUDIO_REASON),
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="b", text="y", audio=Path("b.wav")),
                vp_benchmark.MISSING_AUDIO_REASON),
        ])
        line = vp_benchmark.format_summary_line(
            summary, audio_hint_path="C:/AppData/WhisperDictate/benchmark/audio")

        self.assertEqual(summary["skipped_no_audio"], 2)
        self.assertIn("0/2 passed", line)
        self.assertIn("2 skipped (no audio)", line)
        self.assertIn("record corpus audio to C:/AppData/WhisperDictate/benchmark/audio", line)

    def test_format_summary_line_no_audio_hint_when_some_scored(self):
        from whisper_dictate import vp_benchmark

        # A mix of scored + skipped must NOT trigger the all-skipped audio hint.
        summary = vp_benchmark.summarize_results([
            {"benchmark_success": True, "wer": 0.0, "cer": 0.0},
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="b", text="y", audio=Path("b.wav")),
                vp_benchmark.MISSING_AUDIO_REASON),
        ])
        line = vp_benchmark.format_summary_line(summary, audio_hint_path="X")

        self.assertIn("1 skipped (no audio)", line)
        self.assertNotIn("record corpus audio", line)

    def test_format_summary_line_mixed_skips_shows_no_audio_count_not_all(self):
        from whisper_dictate import vp_benchmark

        # 3 skipped: 2 for missing audio, 1 for another reason.
        # Expected: "3 skipped (2 no audio)" — NOT "3 skipped (no audio)"
        # and no all-skipped audio-hint even if a hint path is given.
        summary = vp_benchmark.summarize_results([
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="a", text="x", audio=Path("a.wav")),
                vp_benchmark.MISSING_AUDIO_REASON),
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="b", text="y", audio=Path("b.wav")),
                vp_benchmark.MISSING_AUDIO_REASON),
            vp_benchmark.skipped_event(
                vp_benchmark.CorpusItem(id="c", text="z", audio=Path("c.wav")),
                "backend unavailable"),
        ])
        line = vp_benchmark.format_summary_line(
            summary, audio_hint_path="C:/AppData/WhisperDictate/benchmark/audio")

        self.assertEqual(summary["skipped"], 3)
        self.assertEqual(summary["skipped_no_audio"], 2)
        # Precise breakdown shown, not the all-audio label.
        self.assertIn("3 skipped (2 no audio)", line)
        self.assertNotIn("3 skipped (no audio)", line)
        # all-skipped hint suppressed because not ALL skips are missing-audio.
        self.assertNotIn("record corpus audio", line)

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

    def test_handle_benchmark_cli_path_passes_appdata_to_run_benchmark(self):
        from whisper_dictate import runtime

        ap = types.SimpleNamespace(error=lambda msg: (_ for _ in ()).throw(
            AssertionError(f"ap.error called: {msg}")))
        args = types.SimpleNamespace(
            run_benchmark=False,  # CLI path — NOT the UI "Run benchmark" button
            benchmark_files=["audio.wav"],
            benchmark_corpus=None,
            benchmark_backends="whisper",
            benchmark_jsonl=None,
        )
        fake_appdata = Path("/fake/appdata")
        # Use patch.object to target the module object directly — avoids a
        # lookup through sys.modules (which may have been cleared by a prior
        # test that pops "whisper_dictate.runtime" in its finally block).
        with patch.object(runtime, "appdata_dir", return_value=fake_appdata):
            with patch("whisper_dictate.vp_benchmark.run_benchmark") as plain:
                plain.return_value = []
                runtime._handle_benchmark(args, ap)

        plain.assert_called_once()
        call_kw = plain.call_args.kwargs
        # The CLI path must thread appdata through so the per-user audio
        # fallback works identically to the UI "Run benchmark" path.
        self.assertEqual(call_kw.get("appdata"), fake_appdata)

class ConfigDirTests(unittest.TestCase):
    def test_appdata_dir_ignores_empty_xdg_config_home_on_non_windows(self):
        """XDG_CONFIG_HOME="" must fall back to ~/.config, not use a
        relative "./whisper-dictate" directory (empty string is not a path)."""
        import platform
        from whisper_dictate import vp_config

        if os.name == "nt":
            self.skipTest("XDG_CONFIG_HOME is a Linux/macOS concern")

        home_default = Path.home() / ".config" / "whisper-dictate"
        old = os.environ.pop("XDG_CONFIG_HOME", None)
        try:
            os.environ["XDG_CONFIG_HOME"] = ""
            result = vp_config.appdata_dir()
        finally:
            os.environ.pop("XDG_CONFIG_HOME", None)
            if old is not None:
                os.environ["XDG_CONFIG_HOME"] = old

        self.assertEqual(result, home_default)

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

    def test_runtime_uses_combined_record_sink_helper(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("append_record_sinks(", script)
        self.assertIn("metrics_jsonl=self.metrics_jsonl", script)
        self.assertIn("json_output=self.json_output", script)

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
