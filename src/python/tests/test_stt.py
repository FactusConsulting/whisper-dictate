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

