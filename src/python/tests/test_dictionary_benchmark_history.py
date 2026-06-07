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

class TranscribeFileTests(unittest.TestCase):
    def _write_test_wav(self, path, *, rate=16000, seconds=0.8):
        import math
        import struct

        frames = int(rate * seconds)
        pcm = b"".join(
            struct.pack("<h", int(0.25 * 32767 * math.sin(2 * math.pi * 440 * i / rate)))
            for i in range(frames)
        )
        with wave.open(path, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(rate)
            wav.writeframes(pcm)

    def test_parser_accepts_transcribe_file(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args(
            ["--transcribe-file", "sample.wav"])
        self.assertEqual(args.transcribe_file, "sample.wav")

    def test_load_audio_file_decodes_wav_as_16khz_int16_mono(self):
        sys.modules["numpy"] = real_numpy()
        sys.modules.pop("whisper_dictate.runtime", None)
        from whisper_dictate import runtime

        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path, rate=8000)
            pcm = runtime.load_audio_file(path)
        finally:
            os.remove(path)

        self.assertEqual(pcm.dtype.name, "int16")
        self.assertEqual(pcm.ndim, 2)
        self.assertEqual(pcm.shape[1], 1)
        self.assertGreaterEqual(len(pcm), 12000)

    def test_transcribe_file_event_uses_dictionary_replacements(self):
        sys.modules["numpy"] = real_numpy()
        for name in ("vp_audio", "vp_transcribe", "whisper_dictate.runtime"):
            sys.modules.pop(name, None)
        from whisper_dictate import runtime
        from whisper_dictate import vp_transcribe

        class Segment:
            text = " lead death"
            start = 0.0
            end = 0.8

        class Info:
            language = "en"
            language_probability = 0.9

        class Model:
            def transcribe(self, *_args, **_kwargs):
                return [Segment()], Info()

        def dictionary_runtime(text="", base_prompt=None):
            if text:
                return vp_transcribe.DictionaryRuntimeResult(
                    text=text.replace("lead death", "lead dev"),
                    prompt=base_prompt,
                    terms=["lead dev"],
                    changes=[{"from": "lead death", "to": "lead dev", "count": 1}],
                    term_count=1,
                    replacement_count=1,
                )
            return vp_transcribe.DictionaryRuntimeResult(
                text=text,
                prompt=base_prompt,
                terms=["lead dev"],
                term_count=1,
                replacement_count=1,
            )

        old_dictionary_runtime = vp_transcribe._dictionary_runtime
        old_gate = vp_transcribe._looks_like_speech
        vp_transcribe._dictionary_runtime = dictionary_runtime
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            event = runtime.transcribe_file_event(
                Model(), path, "en",
                model_name="fake", stt_backend="whisper",
                device="cpu", compute_type="int8",
            )
        finally:
            vp_transcribe._dictionary_runtime = old_dictionary_runtime
            vp_transcribe._looks_like_speech = old_gate
            os.remove(path)

        self.assertEqual(event["event"], "file_transcription")
        self.assertEqual(event["text"], "lead dev")
        self.assertEqual(event["raw_text"], "lead death")
        self.assertEqual(event["source_file"], path)
        self.assertEqual(event["dictionary_terms"], ["lead dev"])
        self.assertEqual(event["dictionary_replacements"][0]["from"], "lead death")

    def test_transcribe_file_json_output_is_single_json_object(self):
        from whisper_dictate import runtime

        event = {"event": "file_transcription", "text": "hello"}
        with _capture_stdout() as buf:
            runtime.print_transcribe_file_result(event, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), event)

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
        self.assertIn('profile=getattr(self, "_active_profile_name", None)', script)

    def test_history_keeps_profile_field(self):
        from whisper_dictate import runtime

        event = {"text": "hello", "profile": "Claude terminal"}
        stored = runtime._history_event(event)

        self.assertEqual(stored["profile"], "Claude terminal")
