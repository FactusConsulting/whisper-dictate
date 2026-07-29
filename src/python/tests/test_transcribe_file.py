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
        # Provenance parity with the live loop: `--transcribe-file` is a
        # supported flow, so its JSON must also say which runtime and which
        # implementation processed the file. `stt_backend`/`device` above
        # are the CONFIGURED values and cannot answer that. Codex P2 #687.
        self.assertEqual(event["engine"], "python-worker")
        # The fake `Model` here exposes neither `stt_provenance()` nor a
        # CTranslate2 `.model.device`, so honest "unknown" is the answer --
        # not a guess.
        self.assertEqual(event["stt_impl"], "unknown")
        self.assertEqual(event["stt_accel"], "unknown")

    def test_transcribe_file_event_reports_the_rust_helper_provenance(self):
        """A model that knows its own provenance (the Rust whisper.cpp
        helper) must have it land on the file-transcription event."""
        sys.modules["numpy"] = real_numpy()
        for name in ("vp_audio", "vp_transcribe", "whisper_dictate.runtime"):
            sys.modules.pop(name, None)
        from whisper_dictate import runtime
        from whisper_dictate import vp_transcribe

        class Segment:
            text = " hello"
            start = 0.0
            end = 0.8

        class Info:
            language = "en"
            language_probability = 0.9

        class Model:
            def transcribe(self, *_args, **_kwargs):
                return [Segment()], Info()

            def stt_provenance(self):
                return ("whisper.cpp", "vulkan")

        def passthrough_dictionary(text="", base_prompt=None):
            return vp_transcribe.DictionaryRuntimeResult(
                text=text, prompt=base_prompt, terms=[],
            )

        # Patch `_dictionary_runtime` (this test is not about the
        # dictionary) AND snapshot the module-level prompt cache: whatever
        # this run memoises under the current cache key would otherwise be
        # served to the sibling dictionary test, whose own patch would then
        # never be consulted.
        old_dictionary_runtime = vp_transcribe._dictionary_runtime
        old_gate = vp_transcribe._looks_like_speech
        old_prompt_cache = dict(vp_transcribe._DICTIONARY_PROMPT_CACHE)
        vp_transcribe._dictionary_runtime = passthrough_dictionary
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            event = runtime.transcribe_file_event(
                Model(), path, "en",
                model_name="fake", stt_backend="whisper",
                device="auto", compute_type="int8",
            )
        finally:
            vp_transcribe._dictionary_runtime = old_dictionary_runtime
            vp_transcribe._looks_like_speech = old_gate
            vp_transcribe._DICTIONARY_PROMPT_CACHE.clear()
            vp_transcribe._DICTIONARY_PROMPT_CACHE.update(old_prompt_cache)
            os.remove(path)

        self.assertEqual(event["engine"], "python-worker")
        self.assertEqual(event["stt_impl"], "whisper.cpp")
        self.assertEqual(event["stt_accel"], "vulkan")
        # The configured labels are unchanged and still ambiguous on their
        # own -- that is exactly why the three new fields exist.
        self.assertEqual(event["stt_backend"], "whisper")
        self.assertEqual(event["device"], "auto")

    def test_transcribe_file_postprocesses_in_the_language_it_transcribed(self):
        """``--transcribe-file`` must stamp the effective language too.

        #686 follow-up (Codex P2): the file path called ``postprocess_text``
        with no settings at all, so the pass reloaded the SAVED config — a
        file transcribed with ``--lang en`` (or auto-detected as English)
        while the saved setting is ``da`` got a Danish cleanup prompt
        ordering the model not to translate an English text.
        """
        sys.modules["numpy"] = real_numpy()
        for name in ("vp_audio", "vp_transcribe", "whisper_dictate.runtime"):
            sys.modules.pop(name, None)
        from whisper_dictate import runtime
        from whisper_dictate import vp_audio_file
        from whisper_dictate import vp_postprocess
        from whisper_dictate import vp_transcribe

        class Segment:
            text = " hello there"
            start = 0.0
            end = 0.8

        class Info:
            language = "en"
            language_probability = 0.9

        class Model:
            def transcribe(self, *_args, **_kwargs):
                return [Segment()], Info()

        seen = []

        def recording_postprocess(text, settings=None):
            seen.append((text, settings))
            return types.SimpleNamespace(
                text=text, provider="none", mode="raw", model="", latency_ms=0,
                changed=False, fallback=False, error=None, redacted=False,
                redactions=[],
            )

        # The SAVED config says Danish; this run transcribed English.
        saved = vp_postprocess.PostprocessSettings(
            processor="ollama", mode="clean", lang="da")
        old_gate = vp_transcribe._looks_like_speech
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            with patch.object(vp_audio_file, "postprocess_text", recording_postprocess), \
                    patch.object(vp_audio_file, "load_postprocess_settings", lambda: saved):
                event = runtime.transcribe_file_event(
                    Model(), path, "en",
                    model_name="fake", stt_backend="whisper",
                    device="cpu", compute_type="int8",
                )
        finally:
            vp_transcribe._looks_like_speech = old_gate
            os.remove(path)

        self.assertEqual(len(seen), 1)
        text, settings = seen[0]
        self.assertEqual(
            settings.lang, "en",
            "the file pass must be told the language it transcribed in",
        )
        prompt = vp_postprocess.build_prompt(text, settings.mode, settings.lang)
        self.assertIn("the input is in en (ISO 639-1 code)", prompt)
        self.assertNotIn("the input is in da", prompt)
        # The record reports the same language the prompt names.
        self.assertEqual(event["language"], "en")

    def test_transcribe_file_json_output_is_single_json_object(self):
        from whisper_dictate import runtime

        event = {"event": "file_transcription", "text": "hello"}
        with _capture_stdout() as buf:
            runtime.print_transcribe_file_result(event, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), event)



class TranscribeRustHelperTests(unittest.TestCase):
    """The Rust-helper + parsing pieces extracted from _assert_local_backend and
    _dictionary_runtime, exercised with subprocess stubbed."""

    def setUp(self):
        from whisper_dictate import vp_transcribe
        self.vp = vp_transcribe

    def _completed(self, returncode=0, stdout=""):
        return types.SimpleNamespace(returncode=returncode, stdout=stdout, stderr="")

    def test_parse_dictionary_changes_keeps_valid_items_and_defaults_count(self):
        payload = {"changes": [
            {"from": "a", "to": "b", "count": 3},
            "not-a-dict",
            {"from": "x", "to": "y"},
        ]}
        self.assertEqual(
            self.vp._parse_dictionary_changes(payload),
            [{"from": "a", "to": "b", "count": 3},
             {"from": "x", "to": "y", "count": 0}],
        )

    def test_run_dictionary_helper_payload_none_without_helper(self):
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("VOICEPI_RUST_INJECTOR", None)
            self.assertIsNone(self.vp._run_dictionary_helper_payload("hi", None))

    def test_run_dictionary_helper_payload_parses_dict(self):
        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "rust"}), \
                patch.object(self.vp.subprocess, "run",
                             return_value=self._completed(0, '{"text": "hi", "enabled": true}')):
            self.assertEqual(
                self.vp._run_dictionary_helper_payload("hi", None),
                {"text": "hi", "enabled": True},
            )

    def test_run_dictionary_helper_payload_none_on_bad_json_or_nonzero(self):
        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "rust"}):
            with patch.object(self.vp.subprocess, "run",
                              return_value=self._completed(0, "not json")):
                self.assertIsNone(self.vp._run_dictionary_helper_payload("hi", None))
            with patch.object(self.vp.subprocess, "run",
                              return_value=self._completed(1, "{}")):
                self.assertIsNone(self.vp._run_dictionary_helper_payload("hi", None))
            with patch.object(self.vp.subprocess, "run", side_effect=OSError("boom")):
                self.assertIsNone(self.vp._run_dictionary_helper_payload("hi", None))

    def test_rust_privacy_ok_true_false_and_raise(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=self._completed(0, '{"ok": true}')):
            self.assertTrue(self.vp._rust_privacy_ok("rust", "whisper", "STT"))
        with patch.object(self.vp.subprocess, "run",
                          return_value=self._completed(0, '{"ok": false, "error": "blocked"}')):
            with self.assertRaises(RuntimeError):
                self.vp._rust_privacy_ok("rust", "openai", "STT")
        with patch.object(self.vp.subprocess, "run",
                          return_value=self._completed(1, "")):
            self.assertFalse(self.vp._rust_privacy_ok("rust", "whisper", "STT"))
        with patch.object(self.vp.subprocess, "run", side_effect=OSError("boom")):
            self.assertFalse(self.vp._rust_privacy_ok("rust", "whisper", "STT"))
