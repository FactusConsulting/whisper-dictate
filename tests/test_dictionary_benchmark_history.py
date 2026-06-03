from tests.test_helpers import *

class DictionarySuggestTests(unittest.TestCase):
    def test_suggests_replacements_from_benchmark_term_misses(self):
        import vp_dictionary_suggest

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
        import vp_dictionary_suggest

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
        import vp_dictionary_suggest

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
        import vp_dictionary_suggest

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
        import vp_dictionary_suggest

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
        import vp_cli

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
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args(
            ["--transcribe-file", "sample.wav"])
        self.assertEqual(args.transcribe_file, "sample.wav")

    def test_load_audio_file_decodes_wav_as_16khz_int16_mono(self):
        sys.modules["numpy"] = real_numpy()
        sys.modules.pop("vp_file_transcribe", None)
        import vp_file_transcribe

        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path, rate=8000)
            pcm = vp_file_transcribe.load_audio_file(path)
        finally:
            os.remove(path)

        self.assertEqual(pcm.dtype.name, "int16")
        self.assertEqual(pcm.ndim, 2)
        self.assertEqual(pcm.shape[1], 1)
        self.assertGreaterEqual(len(pcm), 12000)

    def test_transcribe_file_event_uses_dictionary_replacements(self):
        sys.modules["numpy"] = real_numpy()
        for name in ("vp_audio", "vp_transcribe", "vp_file_transcribe"):
            sys.modules.pop(name, None)
        import vp_file_transcribe
        import vp_transcribe

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

        class Dict:
            def build_prompt(self, prompt):
                return prompt

            def apply_replacements(self, text):
                return text.replace("lead death", "lead dev"), [
                    {"from": "lead death", "to": "lead dev", "count": 1}
                ]

            def prompt_terms(self):
                return ["lead dev"]

        old_dict = vp_transcribe.DICTIONARY
        old_gate = vp_transcribe._looks_like_speech
        vp_transcribe.DICTIONARY = Dict()
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            event = vp_file_transcribe.transcribe_file_event(
                Model(), path, "en",
                model_name="fake", stt_backend="whisper",
                device="cpu", compute_type="int8",
            )
        finally:
            vp_transcribe.DICTIONARY = old_dict
            vp_transcribe._looks_like_speech = old_gate
            os.remove(path)

        self.assertEqual(event["event"], "file_transcription")
        self.assertEqual(event["text"], "lead dev")
        self.assertEqual(event["raw_text"], "lead death")
        self.assertEqual(event["source_file"], path)
        self.assertEqual(event["dictionary_terms"], ["lead dev"])
        self.assertEqual(event["dictionary_replacements"][0]["from"], "lead death")

    def test_transcribe_file_json_output_is_single_json_object(self):
        import vp_file_transcribe

        event = {"event": "file_transcription", "text": "hello"}
        with _capture_stdout() as buf:
            vp_file_transcribe.print_transcribe_file_result(event, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), event)

class BenchmarkTests(unittest.TestCase):
    def test_corpus_manifest_loads_and_scores_terms(self):
        import vp_corpus

        item = vp_corpus.load_corpus("benchmark/corpus.json")[0]

        self.assertEqual(item.id, "da-short-001")
        self.assertTrue(str(item.audio).endswith("benchmark\\audio\\da-short-001.wav") or
                        str(item.audio).endswith("benchmark/audio/da-short-001.wav"))
        self.assertEqual(vp_corpus.wer("Claude Code virker", "Claude virker"), 1 / 3)
        report = vp_corpus.term_report(["Claude Code", "Codex"], "Claude Code works")
        self.assertEqual(report["hits"], ["Claude Code"])
        self.assertEqual(report["misses"], ["Codex"])

    def test_corpus_annotates_benchmark_event(self):
        import vp_corpus

        item = vp_corpus.CorpusItem(
            id="x",
            text="Claude Code and Codex",
            audio=Path("x.wav"),
            language="en",
            category="tech",
            terms=("Claude Code", "Codex"),
        )
        event = vp_corpus.annotate_event({"text": "Claude Code and codec"}, item)

        self.assertEqual(event["corpus_id"], "x")
        self.assertEqual(event["corpus_language"], "en")
        self.assertGreater(event["wer"], 0)
        self.assertEqual(event["term_hits"], ["Claude Code"])
        self.assertEqual(event["term_misses"], ["Codex"])

    def test_parse_backend_specs_supports_models(self):
        import vp_benchmark

        specs = vp_benchmark.parse_backend_specs(
            "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3,openai:gpt-4o-mini-transcribe")

        self.assertEqual(specs[0].backend, "whisper")
        self.assertEqual(specs[0].model, "large-v3")
        self.assertEqual(specs[1].backend, "parakeet")
        self.assertEqual(specs[1].model, "nvidia/parakeet-tdt-0.6b-v3")
        self.assertEqual(specs[2].backend, "openai")
        self.assertEqual(specs[2].model, "gpt-4o-mini-transcribe")

    def test_parse_backend_specs_rejects_unknown_backend(self):
        import vp_benchmark

        with self.assertRaisesRegex(ValueError, "unsupported benchmark backend"):
            vp_benchmark.parse_backend_specs("cloud:gpt-4o-transcribe")

    def test_benchmark_run_one_invokes_transcribe_file_json(self):
        import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=0,
            stdout='{"event":"file_transcription","text":"hello"}\n',
            stderr="",
        )
        with patch("vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="whisper:large-v3", backend="whisper", model="large-v3"),
                python_exe="python",
                app_path="voice_pi.py",
                base_env={},
            )

        cmd = run.call_args.args[0]
        env = run.call_args.kwargs["env"]
        self.assertEqual(cmd, [
            "python", "voice_pi.py", "--transcribe-file", "sample.wav", "--json"
        ])
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "whisper")
        self.assertEqual(env["VOICEPI_MODEL"], "large-v3")
        self.assertTrue(event["benchmark_success"])
        self.assertEqual(event["benchmark_backend_spec"], "whisper:large-v3")

    def test_benchmark_parakeet_model_uses_parakeet_env(self):
        import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=1,
            stdout="",
            stderr="missing nemo",
        )
        with patch("vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="parakeet:nvidia/model", backend="parakeet",
                    model="nvidia/model"),
                python_exe="python",
                app_path="voice_pi.py",
                base_env={},
            )

        env = run.call_args.kwargs["env"]
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "parakeet")
        self.assertEqual(env["VOICEPI_PARAKEET_MODEL"], "nvidia/model")
        self.assertFalse(event["benchmark_success"])
        self.assertIn("missing nemo", event["benchmark_error"])

    def test_benchmark_jsonl_writes_one_line_per_file_backend(self):
        import vp_benchmark

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
            with patch("vp_benchmark.run_one", side_effect=fake_run_one):
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
        import vp_benchmark

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
        import vp_benchmark

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
                with patch("vp_benchmark._load_model_for_spec", return_value=("model", "m", "cpu", "int8")) as load:
                    with patch("vp_file_transcribe.transcribe_file_event", side_effect=fake_transcribe):
                        results = vp_benchmark.run_benchmark(
                            None,
                            "whisper:tiny",
                            corpus_manifest=manifest_path,
                        )
            finally:
                sys.modules.pop("vp_file_transcribe", None)
                sys.modules.pop("vp_transcribe", None)

        self.assertEqual(load.call_count, 1)
        self.assertEqual(len(calls), 2)
        self.assertEqual([c[2] for c in calls], ["da", "en"])
        self.assertTrue(all(r["benchmark_success"] for r in results))
        self.assertEqual(results[1]["term_hits"], ["Claude Code"])

    def test_record_corpus_imports_sounddevice_lazily_with_help(self):
        with open("scripts/record-corpus.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def load_sounddevice", script)
        self.assertIn("Missing recorder dependency: sounddevice", script)
        self.assertIn("py -3.12 -m pip install", script)
        self.assertIn("sounddevice>=0.4,<0.6", script)
        self.assertIn("sd = load_sounddevice()", script)
        self.assertNotIn("\nimport sounddevice as sd\n", script)

    def test_parser_accepts_benchmark_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

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
    def test_append_and_read_history_keeps_core_fields(self):
        import vp_history

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
            vp_history.append_history(event, Path(path))
            rows = vp_history.read_history(10, Path(path))
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
        import vp_history

        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "first"}) + "\n")
            f.write(json.dumps({"text": "second"}) + "\n")
        try:
            item = vp_history.last_history(Path(path))
        finally:
            os.remove(path)

        self.assertEqual(item["text"], "second")

    def test_history_can_be_disabled(self):
        old = os.environ.get("VOICEPI_HISTORY_ENABLED")
        os.environ["VOICEPI_HISTORY_ENABLED"] = "0"
        sys.modules.pop("vp_history", None)
        import vp_history

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            os.remove(path)
            written = vp_history.append_history({"text": "hidden"}, Path(path))
            self.assertIsNone(written)
            self.assertFalse(os.path.exists(path))
        finally:
            if old is None:
                os.environ.pop("VOICEPI_HISTORY_ENABLED", None)
            else:
                os.environ["VOICEPI_HISTORY_ENABLED"] = old
            sys.modules.pop("vp_history", None)

    def test_history_copy_last_uses_clipboard(self):
        import vp_history

        copied = {}

        fake_pyperclip = types.ModuleType("pyperclip")
        fake_pyperclip.copy = lambda text: copied.setdefault("text", text)
        sys.modules["pyperclip"] = fake_pyperclip
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "copy me"}) + "\n")
        try:
            text = vp_history.copy_last_to_clipboard(Path(path))
        finally:
            os.remove(path)
            sys.modules.pop("pyperclip", None)

        self.assertEqual(text, "copy me")
        self.assertEqual(copied["text"], "copy me")

    def test_parser_accepts_history_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

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

    def test_voice_pi_appends_history_after_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("from vp_history import append_history", script)
        self.assertIn("append_history(event)", script)
        self.assertLess(script.index("append_jsonl(self.metrics_jsonl, event)"),
                        script.index("append_history(event)"))

class ProfileTests(unittest.TestCase):
    def test_profile_match_by_title_and_process_applies_settings(self):
        import vp_profiles

        profiles = [{
            "name": "Claude terminal",
            "match": {"title": "Claude Code", "process": "WindowsTerminal"},
            "settings": {"inject_mode": "paste", "lang": "en"},
        }]

        config, name = vp_profiles.apply_profile_settings(
            {"inject_mode": "auto", "lang": "da"},
            profiles,
            title="Claude Code - repo",
            process="WindowsTerminal.exe",
        )

        self.assertEqual(name, "Claude terminal")
        self.assertEqual(config["inject_mode"], "paste")
        self.assertEqual(config["lang"], "en")

    def test_profile_match_returns_default_when_no_match(self):
        import vp_profiles

        config, name = vp_profiles.apply_profile_settings(
            {"inject_mode": "auto"},
            [{"name": "Slack", "match": {"title": "Slack"}, "settings": {"lang": "en"}}],
            title="Codex",
            process="WindowsTerminal.exe",
        )

        self.assertIsNone(name)
        self.assertEqual(config, {"inject_mode": "auto"})

    def test_profile_match_supports_lists(self):
        import vp_profiles

        name, settings = vp_profiles.match_profile(
            [{
                "name": "AI terminals",
                "match": {"title": ["Claude Code", "Codex"]},
                "settings": {"inject_mode": "paste"},
            }],
            title="Codex",
            process=None,
        )

        self.assertEqual(name, "AI terminals")
        self.assertEqual(settings["inject_mode"], "paste")

    def test_voice_pi_records_active_profile_in_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def _profiled_config", script)
        self.assertIn("apply_profile_settings", script)
        self.assertIn("[profile] active:", script)
        self.assertIn('profile=getattr(self, "_active_profile_name", None)', script)

    def test_history_keeps_profile_field(self):
        import vp_history

        event = {"text": "hello", "profile": "Claude terminal"}
        stored = vp_history._history_event(event)

        self.assertEqual(stored["profile"], "Claude terminal")

class DictionaryTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_DICTIONARY", "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_DICTIONARY_MAX_TERMS", "VOICEPI_DICTIONARY_PROMPT_CHARS",
        )}
        sys.modules.pop("vp_dictionary", None)

    def tearDown(self):
        for k in list(self._old):
            os.environ.pop(k, None)
            if self._old[k] is not None:
                os.environ[k] = self._old[k]
        sys.modules.pop("vp_dictionary", None)

    def test_dictionary_json_filename_literal_is_centralized(self):
        source = Path("vp_dictionary.py").read_text(encoding="utf-8")

        self.assertIn('DICTIONARY_JSON_NAME = "dictionary.json"', source)
        self.assertEqual(source.count('"dictionary.json"'), 1)

    def test_json_dictionary_builds_prompt_and_replacements(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Slack","Claude Code","Codex"],'
                    '"replacements":{"Cloud Code":"Claude Code","code X":"Codex"}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            d = vp_dictionary.DICTIONARY
            self.assertEqual(d.prompt_terms(), ["Slack", "Claude Code", "Codex"])
            self.assertIn("Vocabulary: Slack, Claude Code, Codex",
                          d.build_prompt("Base prompt"))
            text, changes = d.apply_replacements("Open Cloud Code and code X.")
            self.assertEqual(text, "Open Claude Code and Codex.")
            self.assertEqual(len(changes), 2)
        finally:
            os.remove(path)

    def test_text_dictionary_supports_simple_sections(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False, encoding="utf-8") as f:
            f.write("terms:\n- OpenClaw\n- GitHub Actions\n\n"
                    "replacements:\nopen claw => OpenClaw\n")
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            d = vp_dictionary.DICTIONARY
            self.assertIn("OpenClaw", d.terms)
            text, _ = d.apply_replacements("start open claw")
            self.assertEqual(text, "start OpenClaw")
        finally:
            os.remove(path)

    def test_invalid_prompt_limits_fall_back_to_defaults(self):
        import vp_dictionary

        os.environ["VOICEPI_DICTIONARY_MAX_TERMS"] = "bogus"
        os.environ["VOICEPI_DICTIONARY_PROMPT_CHARS"] = "bogus"
        d = vp_dictionary.Dictionary(["Slack", "Claude Code"], {})
        with _capture_stdout() as buf:
            self.assertEqual(d.prompt_terms(), ["Slack", "Claude Code"])
        self.assertIn("ignoring invalid VOICEPI_DICTIONARY_MAX_TERMS", buf.getvalue())

    def test_dictionary_add_term_creates_json_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, added = vp_dictionary.add_dictionary_term("Claude Code")
            _, added_again = vp_dictionary.add_dictionary_term("claude code")
            with open(path, encoding="utf-8") as f:
                data = json.load(f)

        self.assertEqual(str(written), path)
        self.assertTrue(added)
        self.assertFalse(added_again)
        self.assertEqual(data["terms"], ["Claude Code"])
        self.assertEqual(data["replacements"], {})

    def test_dictionary_add_replacement_preserves_terms(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Codex"],"replacements":{}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, src, dst, changed = vp_dictionary.add_dictionary_replacement(
                "code X=Codex")
            with open(path, encoding="utf-8") as f:
                data = json.load(f)
        finally:
            os.remove(path)

        self.assertEqual(str(written), path)
        self.assertEqual((src, dst, changed), ("code X", "Codex", True))
        self.assertEqual(data["terms"], ["Codex"])
        self.assertEqual(data["replacements"], {"code X": "Codex"})

    def test_dictionary_add_replacements_preserves_terms_and_counts_changes(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Codex"],"replacements":{"old":"Old"}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, changed = vp_dictionary.add_dictionary_replacements({
                "code X": "Codex",
                "old": "Old",
                "": "ignored",
            })
            with open(path, encoding="utf-8") as f:
                data = json.load(f)
        finally:
            os.remove(path)

        self.assertEqual(str(written), path)
        self.assertEqual(changed, 1)
        self.assertEqual(data["terms"], ["Codex"])
        self.assertEqual(data["replacements"], {"code X": "Codex", "old": "Old"})
