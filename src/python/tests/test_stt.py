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
)

class HallucinationFilterTests(unittest.TestCase):
    """is_hallucination filters Whisper's known output when fed near-silence."""

    def setUp(self):
        # Pure import — no numpy / faster_whisper needed for this surface.
        for n in ("vp_transcribe", "vp_audio",
                  "whisper_dictate.vp_transcribe", "whisper_dictate.vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        from whisper_dictate import vp_transcribe
        self.t = vp_transcribe

    def test_known_hallucination_filtered(self):
        for phrase in ("tak", "Tak.", "TAK FORDI DU SÅ MED",
                       "thank you for watching", "Undertekster af"):
            self.assertTrue(self.t.is_hallucination(phrase),
                            f"{phrase!r} should match")

    def test_trailing_whitespace_still_matches(self):
        self.assertTrue(self.t.is_hallucination("tak.  \n"))

    def test_genuine_text_not_filtered(self):
        for phrase in ("hello world", "tak for hjælpen",
                       "dette er en sætning der ikke er hallucination"):
            self.assertFalse(self.t.is_hallucination(phrase),
                             f"{phrase!r} should NOT match")

    def test_subtitle_credit_patterns_filtered(self):
        # The motivating real-world repro plus other named-credit shapes the
        # exact-match blacklist can't enumerate. Anchored full-text match.
        # Phrase-forms require a trailing 4-digit year; company names are
        # specific enough to match without one.
        for phrase in (
            "Danske tekster af Jesper Buhl Scandinavian Text Service 2018",
            "Undertekster af Jesper Buhl 2019.",
            "Tekstet af Jesper Buhl 2020",
            "Subtitles by John Doe 2019",
            "Scandinavian Text Service",
            "Scandinavian Text Service 2018",
            "Broadcast Text International 2020",
            "Dansk Videotekst",
            "Dansk Video Tekst 2017",
        ):
            self.assertTrue(self.t.is_hallucination(phrase),
                            f"{phrase!r} should match credit pattern")

    def test_phrase_credit_without_year_not_filtered(self):
        # Phrase-forms without a trailing year must NOT match — real dictation
        # can legitimately start with these phrases.  Year-less short clips are
        # caught by the speech-rate gate instead.
        for phrase in (
            "oversat af Google Translate",
            "oversat af en professionel",
            "tekster af sange er svære at huske",
            "undertekster af denne film mangler",
            "subtitles by the way are missing",
            "translated by hand is better",
            "captions by default are off",
            "danske tekster af høj kvalitet",
            "Undertekster af Jesper Buhl",
            "Tekstet af en eller anden",
            "Oversat af nogen",
            "Subtitles by Someone",
            "Subtitled by Someone Else",
            "Captions by ACME",
            "Translated by ACME Corp",
        ):
            self.assertFalse(self.t.is_hallucination(phrase),
                             f"{phrase!r} should NOT match (no trailing year)")

    def test_credit_pattern_does_not_match_real_sentences(self):
        # A real dictation that merely CONTAINS a credit phrase mid-sentence must
        # survive — the pattern is anchored to the WHOLE text.
        for phrase in (
            "jeg leverede tekster af høj kvalitet til kunden i dag",
            "vi talte om hvem der har oversat af gammel litteratur",
            "the subtitles by themselves were not the problem we discussed",
            "scandinavian text service was a company i once worked with closely",
        ):
            self.assertFalse(self.t.is_hallucination(phrase),
                             f"{phrase!r} should NOT match credit pattern")

    def test_repro_string_dropped_by_pattern_filter(self):
        # Guard 2 independently: the 0.31 s repro text is a credit match.
        repro = "Danske tekster af Jesper Buhl Scandinavian Text Service 2018"
        self.assertTrue(self.t.is_hallucination(repro))

    def test_speech_rate_gate_drops_impossible_rate(self):
        # Guard 3 independently: 60 chars in 0.31 s = ~193 chars/s >> 30.
        repro = "Danske tekster af Jesper Buhl Scandinavian Text Service 2018"
        self.t.MAX_CHARS_PER_SECOND = 30.0
        with _capture_stdout() as out:
            self.assertTrue(self.t._exceeds_speech_rate(repro, 0.31))
        self.assertIn("chars/s", out.getvalue())
        self.assertIn("hallucination guard", out.getvalue())

    def test_speech_rate_gate_keeps_normal_speech(self):
        self.t.MAX_CHARS_PER_SECOND = 30.0
        # 8 chars on a 0.5 s clip = 16 chars/s — well under the cap.
        self.assertFalse(self.t._exceeds_speech_rate("ja tak da", 0.5))
        # A real long sentence at a plausible rate survives.
        sentence = "dette er en helt almindelig sætning som jeg siger"
        self.assertFalse(self.t._exceeds_speech_rate(sentence, 4.0))

    def test_speech_rate_gate_disabled_at_zero(self):
        self.t.MAX_CHARS_PER_SECOND = 0.0
        repro = "Danske tekster af Jesper Buhl Scandinavian Text Service 2018"
        self.assertFalse(self.t._exceeds_speech_rate(repro, 0.31))

    def test_drop_hallucinated_segments_removes_trailing_silence(self):
        real = types.SimpleNamespace(
            text=" real speech", start=0.0, end=34.6,
            avg_logprob=-0.19, no_speech_prob=0.006)
        # The classic "like and subscribe" tail: the model flags it as likely
        # non-speech (high no_speech_prob + low confidence) AND its end (64.6s)
        # runs past the 34.6s recording.
        halluc = types.SimpleNamespace(
            text=" like and subscribe", start=34.6, end=64.6,
            avg_logprob=-0.56, no_speech_prob=0.66)
        kept, dropped = self.t._drop_hallucinated_segments([real, halluc], 34.6)
        self.assertEqual([s.text for s in kept], [" real speech"])
        self.assertEqual([s.text for s in dropped], [" like and subscribe"])

    def test_drop_hallucinated_segments_keeps_real_in_bounds_speech(self):
        # Real speech (low no_speech_prob, end within the recording) is kept even
        # if it contains repetition — that in-segment artifact is not what this
        # trailing-silence scrub targets.
        seg = types.SimpleNamespace(
            text=" virkelig virkelig virkelig", start=0.0, end=19.4,
            avg_logprob=-0.18, no_speech_prob=0.0006)
        kept, dropped = self.t._drop_hallucinated_segments([seg], 19.6)
        self.assertEqual(len(kept), 1)
        self.assertEqual(dropped, [])

    def test_drop_hallucinated_segments_requires_both_silence_signals(self):
        # High no_speech_prob alone (but good confidence, in-bounds end) is NOT
        # dropped — avoids nuking quiet-but-real speech.
        quiet_real = types.SimpleNamespace(
            text=" quiet real", start=0.0, end=5.0,
            avg_logprob=-0.2, no_speech_prob=0.7)
        kept, dropped = self.t._drop_hallucinated_segments([quiet_real], 5.0)
        self.assertEqual(len(kept), 1)
        self.assertEqual(dropped, [])

    def test_drop_per_segment_trailing_credit_real_repro(self):
        # REAL REPRO: a 25.9 s dictation, correct across 3 segments, with a
        # trailing 0.42 s segment hallucinating the classic subtitle credit.
        # The trailing segment has no_speech 0.63 but logprob only -0.43, so the
        # plain no_speech+logprob gate does NOT fire (the AND is not satisfied);
        # the credit-pattern drop must catch it instead.
        s1 = types.SimpleNamespace(
            text=("Indtil nu har det været okay, men nu vil jeg gerne kunne "
                  "diktere i klartekst."),
            start=0.34, end=6.36, avg_logprob=-0.162, no_speech_prob=0.0031)
        s2 = types.SimpleNamespace(
            text=("Jeg kunne godt tænke mig at vide om en tast er en valid "
                  "hotkey eller ej."),
            start=6.5, end=17.12, avg_logprob=-0.162, no_speech_prob=0.0031)
        s3 = types.SimpleNamespace(
            text="Eller man kan vælge at diktere ind i et klartekstfelt.",
            start=17.54, end=25.43, avg_logprob=-0.162, no_speech_prob=0.0031)
        credit = types.SimpleNamespace(
            text="Danske tekster af Jesper Buhl Scandinavian Text Service 2018",
            start=25.43, end=25.85, avg_logprob=-0.4286,
            no_speech_prob=0.6338, compression_ratio=0.909)
        kept, dropped = self.t._drop_hallucinated_segments(
            [s1, s2, s3, credit], 25.9)
        self.assertEqual([s.text for s in kept], [s1.text, s2.text, s3.text])
        self.assertEqual([s.text for s in dropped], [credit.text])
        assembled = " ".join(s.text for s in kept)
        self.assertNotIn("Scandinavian Text Service", assembled)
        self.assertNotIn("2018", assembled)

    def test_drop_per_segment_credit_with_high_no_speech(self):
        # A credit-shaped segment the model also flags as non-speech (high
        # no_speech_prob >= NO_SPEECH_DROP) is dropped, even when avg_logprob is
        # not low enough for the plain silence gate. The credit-pattern drop is
        # gated on the corroborating silence signal.
        real = types.SimpleNamespace(
            text="dette er en helt almindelig sætning",
            start=0.0, end=5.0, avg_logprob=-0.15, no_speech_prob=0.01)
        credit = types.SimpleNamespace(
            text="Undertekster af Jesper Buhl 2019",
            start=5.0, end=7.0, avg_logprob=-0.15, no_speech_prob=0.65)
        kept, dropped = self.t._drop_hallucinated_segments([real, credit], 7.5)
        self.assertEqual([s.text for s in kept], [real.text])
        self.assertEqual([s.text for s in dropped], [credit.text])

    def test_drop_per_segment_credit_shape_with_low_no_speech_survives(self):
        # FALSE-POSITIVE GUARD: a credit-SHAPED segment the model is confident
        # is real speech (low no_speech_prob, healthy logprob) must NOT be
        # dropped. Text shape alone must never drop a segment.
        real = types.SimpleNamespace(
            text="dette er en helt almindelig sætning",
            start=0.0, end=5.0, avg_logprob=-0.15, no_speech_prob=0.01)
        credit_shaped = types.SimpleNamespace(
            text="Undertekster af Jesper Buhl 2019",
            start=5.0, end=7.0, avg_logprob=-0.1, no_speech_prob=0.05)
        kept, dropped = self.t._drop_hallucinated_segments(
            [real, credit_shaped], 7.5)
        self.assertEqual(
            [s.text for s in kept], [real.text, credit_shaped.text])
        self.assertEqual(dropped, [])

    def test_drop_per_segment_keeps_confident_year_dictation_da(self):
        # FALSE-POSITIVE REPRO: real, confident Danish dictation that happens to
        # match the year-anchored credit shape must survive.
        seg = types.SimpleNamespace(
            text="oversat af Google i 2023",
            start=0.0, end=3.0, avg_logprob=-0.1, no_speech_prob=0.05)
        kept, dropped = self.t._drop_hallucinated_segments([seg], 3.5)
        self.assertEqual([s.text for s in kept], [seg.text])
        self.assertEqual(dropped, [])

    def test_drop_per_segment_keeps_confident_year_dictation_en(self):
        # FALSE-POSITIVE REPRO (English): confident speech matching the credit
        # shape survives.
        seg = types.SimpleNamespace(
            text="translated by the committee in 2020",
            start=0.0, end=3.0, avg_logprob=-0.1, no_speech_prob=0.05)
        kept, dropped = self.t._drop_hallucinated_segments([seg], 3.5)
        self.assertEqual([s.text for s in kept], [seg.text])
        self.assertEqual(dropped, [])

    def test_drop_per_segment_keeps_segment_containing_credit_phrase(self):
        # A real speech segment that merely CONTAINS "tekster af" mid-text (no
        # trailing year, not anchored) must survive — the credit drop is anchored.
        real = types.SimpleNamespace(
            text="jeg leverede tekster af høj kvalitet til kunden i dag",
            start=0.0, end=4.0, avg_logprob=-0.2, no_speech_prob=0.05)
        kept, dropped = self.t._drop_hallucinated_segments([real], 4.5)
        self.assertEqual([s.text for s in kept], [real.text])
        self.assertEqual(dropped, [])

    def test_drop_per_segment_keeps_real_segments_unchanged(self):
        # Sanity: the 3 real repro segments on their own (no credit appended)
        # are all kept — the per-segment credit check never matches real speech.
        s1 = types.SimpleNamespace(
            text="Indtil nu har det været okay, men nu vil jeg gerne diktere.",
            start=0.34, end=6.36, avg_logprob=-0.162, no_speech_prob=0.0031)
        s2 = types.SimpleNamespace(
            text="Jeg kunne godt tænke mig at vide om en tast er en valid hotkey.",
            start=6.5, end=17.12, avg_logprob=-0.162, no_speech_prob=0.0031)
        s3 = types.SimpleNamespace(
            text="Eller man kan vælge at diktere ind i et klartekstfelt.",
            start=17.54, end=25.43, avg_logprob=-0.162, no_speech_prob=0.0031)
        kept, dropped = self.t._drop_hallucinated_segments([s1, s2, s3], 25.9)
        self.assertEqual(len(kept), 3)
        self.assertEqual(dropped, [])

    def test_drop_per_segment_records_reason(self):
        credit = types.SimpleNamespace(
            text="Danske tekster af Jesper Buhl Scandinavian Text Service 2018",
            start=0.0, end=0.4, avg_logprob=-0.4286, no_speech_prob=0.6338)
        _kept, dropped = self.t._drop_hallucinated_segments([credit], 25.9)
        self.assertEqual(len(dropped), 1)
        self.assertEqual(getattr(dropped[0], "_drop_reason", None), "credit_pattern")

    # --- pattern-data extraction guards (data file <-> code parity) ---

    def test_pattern_data_loadable_via_importlib_resources(self):
        # Packaging-regression guard: the JSON must ship and load through
        # importlib.resources (the mechanism the module uses at import) — a
        # filesystem-path read would mask a missing wheel/zip/installer entry.
        from importlib import resources
        raw = (
            resources.files("whisper_dictate.data")
            .joinpath("hallucination_patterns.json")
            .read_text(encoding="utf-8")
        )
        data = json.loads(raw)
        for key in ("exact_blacklist", "credit_phrase_year_tail",
                    "credit_phrase_prefixes", "bare_company_names"):
            self.assertIn(key, data, f"data file missing key {key!r}")
        self.assertGreater(len(data["exact_blacklist"]), 0)
        self.assertGreater(len(data["credit_phrase_prefixes"]), 0)
        self.assertGreater(len(data["bare_company_names"]), 0)

    def test_hallucinations_set_matches_data_exact_blacklist(self):
        # The compiled frozenset must equal exactly the data file's list — proves
        # the extraction is data-driven (no stray inline literal left behind).
        data = self.t._HALLUCINATION_PATTERNS
        self.assertEqual(self.t.HALLUCINATIONS, frozenset(data["exact_blacklist"]))

    def test_every_data_exact_blacklist_entry_is_hallucination(self):
        # Every previously-hardcoded exact phrase still classifies as a
        # hallucination after the move to the data file.
        for phrase in self.t._HALLUCINATION_PATTERNS["exact_blacklist"]:
            self.assertTrue(self.t.is_hallucination(phrase),
                            f"{phrase!r} (from data file) should match")

    def test_data_driven_credit_examples_still_match(self):
        # Build credit examples straight from the data lists so a future edit to
        # the JSON keeps proving end-to-end that the loaded patterns fire.
        data = self.t._HALLUCINATION_PATTERNS
        # Each phrase prefix + " noget 2019" (a year-terminated tail) must match.
        for prefix in data["credit_phrase_prefixes"]:
            # Resolve simple regex alternations/optionals to a plausible literal
            # by exercising the compiled regex rather than the raw source.
            sample = {
                "(?:danske |norske |svenske )?(?:under)?tekster (?:af|by|:)":
                    "Danske tekster af Jesper Buhl 2019",
                "tekstet af ": "Tekstet af Jesper Buhl 2019",
                "oversat af ": "Oversat af Jesper Buhl 2019",
                "subtitles? by ": "Subtitles by John Doe 2019",
                "subtitled by ": "Subtitled by John Doe 2019",
                "captions? by ": "Captions by John Doe 2019",
                "translated by ": "Translated by John Doe 2019",
            }.get(prefix)
            if sample is None:
                continue  # a newly-added prefix without a hand-written sample
            self.assertTrue(self.t.is_hallucination(sample),
                            f"{sample!r} (prefix {prefix!r}) should match")

    def test_data_driven_bare_company_names_match_without_year(self):
        # Bare company names are specific enough to match with no trailing year.
        company_samples = {
            "scandinavian text service(?: (?:19|20)\\d{2})?": "Scandinavian Text Service",
            "broadcast text international(?: (?:19|20)\\d{2})?": "Broadcast Text International",
            "dansk video ?tekst(?: (?:19|20)\\d{2})?": "Dansk Videotekst",
        }
        for source in self.t._HALLUCINATION_PATTERNS["bare_company_names"]:
            sample = company_samples.get(source)
            if sample is None:
                continue
            self.assertTrue(self.t.is_hallucination(sample),
                            f"{sample!r} (company {source!r}) should match")
            self.assertTrue(self.t.is_hallucination(sample + " 2018"),
                            f"{sample!r} + year should also match")

class TranscribeDetailTests(unittest.TestCase):
    def setUp(self):
        try:
            np = real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        for n in ("vp_transcribe", "vp_audio",
                  "whisper_dictate.vp_transcribe", "whisper_dictate.vp_audio"):
            sys.modules.pop(n, None)
        pkg = sys.modules.get("whisper_dictate")
        if pkg is not None:
            for attr in ("vp_transcribe", "vp_audio"):
                if hasattr(pkg, attr):
                    delattr(pkg, attr)
        sys.modules["numpy"] = np
        from whisper_dictate import vp_transcribe
        self.t = vp_transcribe
        self.np = np

    def test_transcribe_detail_collects_metadata_and_vad_settings(self):
        np = self.np

        class Segment:
            text = " hej"
            start = 0.0
            end = 1.0
            avg_logprob = -0.1
            no_speech_prob = 0.02
            compression_ratio = 1.1

        class Info:
            language = "da"
            language_probability = 0.98

        class Model:
            def __init__(self):
                self.kwargs = None

            def transcribe(self, audio, **kwargs):
                self.kwargs = kwargs
                return [Segment()], Info()

        audio = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(40)
        ]).reshape(-1, 1)
        pcm = (audio * 32767).astype(np.int16)
        model = Model()

        with _capture_stdout():
            result = self.t._transcribe_detail(model, pcm, "da")

        self.assertEqual(result.text, "hej")
        self.assertEqual(result.language, "da")
        self.assertEqual(result.language_probability, 0.98)
        self.assertGreaterEqual(result.compute_s, 0)
        self.assertIsNotNone(result.real_time_factor)
        self.assertEqual(result.segments[0]["avg_logprob"], -0.1)
        self.assertEqual(
            model.kwargs["vad_parameters"]["threshold"],
            self.t.VAD_THRESHOLD,
        )
        # Hallucination guard is on by default → word timestamps + silence-skip
        # threshold are passed so faster-whisper drops silent hallucinated gaps.
        self.assertTrue(self.t.HALLUCINATION_GUARD)
        self.assertTrue(model.kwargs.get("word_timestamps"))
        self.assertEqual(
            model.kwargs.get("hallucination_silence_threshold"),
            self.t.HALLUCINATION_SILENCE_S,
        )

class STTBackendTests(unittest.TestCase):
    def _drop_package_module(self, name):
        sys.modules.pop(name, None)
        package = sys.modules.get("whisper_dictate")
        attr = name.rsplit(".", 1)[-1]
        if package is not None and hasattr(package, attr):
            delattr(package, attr)

    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_STT_BACKEND", "VOICEPI_MODEL", "VOICEPI_PARAKEET_MODEL",
            "VOICEPI_STT_BASE_URL", "VOICEPI_STT_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
        for n in ("whisper_dictate.vp_transcribe", "whisper_dictate.vp_audio",
                  "whisper_dictate.vp_parakeet"):
            self._drop_package_module(n)
        for n in list(sys.modules):
            if (n in ("vp_transcribe", "vp_audio", "vp_parakeet",
                      "faster_whisper", "nemo")
                    or n.startswith("nemo.")):
                sys.modules.pop(n, None)

    def tearDown(self):
        for k, v in self._old.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v
        for n in ("whisper_dictate.vp_transcribe", "whisper_dictate.vp_parakeet"):
            self._drop_package_module(n)
        for n in list(sys.modules):
            if n in ("vp_transcribe", "vp_parakeet") or n.startswith("nemo."):
                sys.modules.pop(n, None)

    def test_default_backend_loads_faster_whisper_without_nemo(self):
        created = {}
        fw = types.ModuleType("faster_whisper")

        class WhisperModel:
            def __init__(self, model_name, *, device, compute_type):
                created["args"] = (model_name, device, compute_type)

        fw.WhisperModel = WhisperModel
        sys.modules["faster_whisper"] = fw
        sys.modules["numpy"] = types.ModuleType("numpy")

        from whisper_dictate import vp_transcribe

        model = vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

        self.assertIsInstance(model, WhisperModel)
        self.assertEqual(created["args"], ("large-v3-turbo", "cpu", "int8"))
        self.assertNotIn("nemo.collections.asr", sys.modules)

    def test_invalid_backend_is_rejected(self):
        os.environ["VOICEPI_STT_BACKEND"] = "bogus"
        sys.modules["numpy"] = types.ModuleType("numpy")
        from whisper_dictate import vp_transcribe

        with self.assertRaisesRegex(ValueError, "VOICEPI_STT_BACKEND"):
            vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

    def test_parakeet_missing_deps_error_is_actionable(self):
        os.environ["VOICEPI_STT_BACKEND"] = "parakeet"
        sys.modules["numpy"] = types.ModuleType("numpy")
        from whisper_dictate import vp_transcribe

        real_import = __import__

        def fake_import(name, *args, **kwargs):
            if name == "nemo.collections.asr" or name.startswith("nemo"):
                raise ImportError("no nemo")
            return real_import(name, *args, **kwargs)

        with patch("builtins.__import__", side_effect=fake_import):
            with self.assertRaisesRegex(RuntimeError, "requirements/parakeet.txt"):
                vp_transcribe.load_stt_model("large-v3-turbo", "cuda", "float16")

    def test_openai_backend_uses_external_transcription_adapter(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_STT_API_KEY"] = "test-key"
        from whisper_dictate import vp_transcribe
        from whisper_dictate import vp_external_api

        with patch.object(vp_external_api.ExternalTranscriptionModel, "__init__", return_value=None) as init:
            model = vp_transcribe.load_stt_model("gpt-4o-mini-transcribe", "cpu", "int8")

        self.assertIsInstance(model, vp_external_api.ExternalTranscriptionModel)
        init.assert_called_once_with("gpt-4o-mini-transcribe")

    def test_local_only_blocks_openai_stt_backend(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        from whisper_dictate import vp_transcribe

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_transcribe.load_stt_model("gpt-4o-mini-transcribe", "cpu", "int8")

    def test_local_only_allows_loopback_self_hosted_endpoint(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        os.environ.pop("VOICEPI_RUST_INJECTOR", None)  # use the Python fallback path
        from whisper_dictate import vp_transcribe

        # A loopback self-hosted endpoint is local → not blocked.
        os.environ["VOICEPI_STT_BASE_URL"] = "http://localhost:8000/v1"
        vp_transcribe._assert_local_backend("openai")
        # A hosted endpoint under local-only is still blocked.
        os.environ["VOICEPI_STT_BASE_URL"] = "https://api.openai.com/v1"
        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_transcribe._assert_local_backend("openai")

    def test_is_loopback_url_classifies_hosts(self):
        from whisper_dictate import vp_transcribe
        for url in ("http://localhost:8000/v1", "http://127.0.0.1/v1",
                    "http://[::1]:9000/v1", "https://127.0.0.5",
                    "http://user:pass@localhost:8000/v1"):
            self.assertTrue(vp_transcribe._is_loopback_url(url), url)
        for url in ("https://api.openai.com/v1", "http://example.com",
                    "http://10.0.0.5:8000", "", None):
            self.assertFalse(vp_transcribe._is_loopback_url(url), str(url))

    def test_parakeet_adapter_uses_nemo_stub_and_default_model(self):
        calls = {}

        fake_np = types.ModuleType("numpy")
        fake_np.float32 = object()
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: True)
        sys.modules["torch"] = torch

        class FakeNemoModel:
            def to(self, device):
                calls["device"] = device

            def eval(self):
                calls["eval"] = True

            def freeze(self):
                calls["freeze"] = True

            def transcribe(self, paths, batch_size=1):
                calls["path"] = paths[0]
                calls["path_exists_during_call"] = os.path.exists(paths[0])
                calls["batch_size"] = batch_size
                return [" hello"]

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                calls["model_name"] = model_name
                return FakeNemoModel()

        nemo = types.ModuleType("nemo")
        collections = types.ModuleType("nemo.collections")
        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        collections.asr = asr
        nemo.collections = collections
        sys.modules["nemo"] = nemo
        sys.modules["nemo.collections"] = collections
        sys.modules["nemo.collections.asr"] = asr

        from whisper_dictate import vp_parakeet
        model = vp_parakeet.ParakeetModel("large-v3-turbo", device="cuda")
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name

        class FakeAudio:
            def reshape(self, *_args):
                return self

            def astype(self, *_args):
                return self

        with patch.object(vp_parakeet, "_write_wav", return_value=path):
            segments, info = model.transcribe(FakeAudio())

        self.assertEqual(
            calls["model_name"], "nvidia/parakeet-tdt-0.6b-v3")
        self.assertEqual(calls["device"], "cuda")
        self.assertTrue(calls["eval"])
        self.assertTrue(calls["freeze"])
        self.assertTrue(calls["path_exists_during_call"])
        self.assertFalse(os.path.exists(calls["path"]))
        self.assertEqual(calls["batch_size"], 1)
        self.assertEqual(segments[0].text, "hello")
        self.assertIsNone(info.language)

    def test_parakeet_ignores_whisper_model_names_without_explicit_override(self):
        calls = {}
        fake_np = types.ModuleType("numpy")
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: True)
        sys.modules["torch"] = torch

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                calls["model_name"] = model_name
                return types.SimpleNamespace()

        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        sys.modules["nemo"] = types.ModuleType("nemo")
        sys.modules["nemo.collections"] = types.ModuleType("nemo.collections")
        sys.modules["nemo.collections.asr"] = asr

        from whisper_dictate import vp_parakeet

        vp_parakeet.ParakeetModel("large-v3", device="cuda")

        self.assertEqual(
            calls["model_name"], "nvidia/parakeet-tdt-0.6b-v3")

    def test_parakeet_cuda_requires_cuda_enabled_torch(self):
        fake_np = types.ModuleType("numpy")
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: False)
        sys.modules["torch"] = torch

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                return types.SimpleNamespace()

        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        sys.modules["nemo"] = types.ModuleType("nemo")
        sys.modules["nemo.collections"] = types.ModuleType("nemo.collections")
        sys.modules["nemo.collections.asr"] = asr

        from whisper_dictate import vp_parakeet

        with self.assertRaisesRegex(RuntimeError, "CUDA-enabled PyTorch"):
            vp_parakeet.ParakeetModel("large-v3", device="cuda")

    def test_parakeet_accepts_explicit_nvidia_model_name(self):
        from whisper_dictate import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("nvidia/custom-parakeet"),
            "nvidia/custom-parakeet",
        )

    def test_parakeet_env_override_wins_over_whisper_model_name(self):
        os.environ["VOICEPI_PARAKEET_MODEL"] = "nvidia/explicit-parakeet"
        from whisper_dictate import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("large-v3"),
            "nvidia/explicit-parakeet",
        )

    def test_parakeet_model_dropdown_options_are_exported(self):
        from whisper_dictate import vp_parakeet

        self.assertEqual(vp_parakeet.PARAKEET_MODELS[0], vp_parakeet.DEFAULT_MODEL)
        self.assertEqual(vp_parakeet.PARAKEET_MODELS, [
            "nvidia/parakeet-tdt-0.6b-v3",
            "nvidia/parakeet-tdt-1.1b",
            "nvidia/parakeet-tdt-0.6b-v2",
        ])

    def test_parakeet_suppresses_irrelevant_pydub_ffmpeg_warning(self):
        from whisper_dictate import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("warnings.filterwarnings", script)
        self.assertIn("Couldn't find ffmpeg or avconv", script)

    def test_parakeet_quiets_nemo_output_unless_stt_debug_is_enabled(self):
        from whisper_dictate import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("def _nemo_output_context", script)
        self.assertIn('os.environ.get("VOICEPI_STT_DEBUG")', script)
        self.assertIn("contextlib.redirect_stdout", script)
        self.assertIn("contextlib.redirect_stderr", script)
        self.assertIn("with _nemo_output_context():", script)

    def test_parakeet_model_load_and_transcribe_are_quieted(self):
        from whisper_dictate import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        load = script.index("self._model = nemo_asr.models.ASRModel.from_pretrained")
        transcribe = script.index("result = self._call_transcribe(path)")
        self.assertLess(script.rfind("with _nemo_output_context():", 0, load), load)
        self.assertLess(script.rfind("with _nemo_output_context():", 0, transcribe), transcribe)

