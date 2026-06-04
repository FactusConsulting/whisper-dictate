from tests.test_helpers import (
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
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        import vp_transcribe
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

class TranscribeDetailTests(unittest.TestCase):
    def setUp(self):
        try:
            np = real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules["numpy"] = np
        import vp_transcribe
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

class STTBackendTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_STT_BACKEND", "VOICEPI_MODEL", "VOICEPI_PARAKEET_MODEL",
            "VOICEPI_STT_BASE_URL", "VOICEPI_STT_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
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

        import vp_transcribe

        model = vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

        self.assertIsInstance(model, WhisperModel)
        self.assertEqual(created["args"], ("large-v3-turbo", "cpu", "int8"))
        self.assertNotIn("nemo.collections.asr", sys.modules)

    def test_invalid_backend_is_rejected(self):
        os.environ["VOICEPI_STT_BACKEND"] = "bogus"
        sys.modules["numpy"] = types.ModuleType("numpy")
        import vp_transcribe

        with self.assertRaisesRegex(ValueError, "VOICEPI_STT_BACKEND"):
            vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

    def test_parakeet_missing_deps_error_is_actionable(self):
        os.environ["VOICEPI_STT_BACKEND"] = "parakeet"
        sys.modules["numpy"] = types.ModuleType("numpy")
        import vp_transcribe

        real_import = __import__

        def fake_import(name, *args, **kwargs):
            if name == "nemo.collections.asr" or name.startswith("nemo"):
                raise ImportError("no nemo")
            return real_import(name, *args, **kwargs)

        with patch("builtins.__import__", side_effect=fake_import):
            with self.assertRaisesRegex(RuntimeError, "requirements-parakeet.txt"):
                vp_transcribe.load_stt_model("large-v3-turbo", "cuda", "float16")

    def test_openai_backend_uses_external_transcription_adapter(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_STT_API_KEY"] = "test-key"
        import vp_transcribe
        import vp_external_api

        with patch.object(vp_external_api.ExternalTranscriptionModel, "__init__", return_value=None) as init:
            model = vp_transcribe.load_stt_model("gpt-4o-mini-transcribe", "cpu", "int8")

        self.assertIsInstance(model, vp_external_api.ExternalTranscriptionModel)
        init.assert_called_once_with("gpt-4o-mini-transcribe")

    def test_local_only_blocks_openai_stt_backend(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_transcribe

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

        import vp_parakeet
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

        import vp_parakeet

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

        import vp_parakeet

        with self.assertRaisesRegex(RuntimeError, "CUDA-enabled PyTorch"):
            vp_parakeet.ParakeetModel("large-v3", device="cuda")

    def test_parakeet_accepts_explicit_nvidia_model_name(self):
        import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("nvidia/custom-parakeet"),
            "nvidia/custom-parakeet",
        )

    def test_parakeet_env_override_wins_over_whisper_model_name(self):
        os.environ["VOICEPI_PARAKEET_MODEL"] = "nvidia/explicit-parakeet"
        import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("large-v3"),
            "nvidia/explicit-parakeet",
        )

    def test_parakeet_model_dropdown_options_are_exported(self):
        import vp_parakeet

        self.assertEqual(vp_parakeet.PARAKEET_MODELS[0], vp_parakeet.DEFAULT_MODEL)
        self.assertEqual(vp_parakeet.PARAKEET_MODELS, [
            "nvidia/parakeet-tdt-0.6b-v3",
            "nvidia/parakeet-tdt-1.1b",
            "nvidia/parakeet-tdt-0.6b-v2",
        ])

    def test_parakeet_suppresses_irrelevant_pydub_ffmpeg_warning(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("warnings.filterwarnings", script)
        self.assertIn("Couldn't find ffmpeg or avconv", script)

    def test_parakeet_quiets_nemo_output_unless_stt_debug_is_enabled(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("def _nemo_output_context", script)
        self.assertIn('os.environ.get("VOICEPI_STT_DEBUG")', script)
        self.assertIn("contextlib.redirect_stdout", script)
        self.assertIn("contextlib.redirect_stderr", script)
        self.assertIn("with _nemo_output_context():", script)

    def test_parakeet_model_load_and_transcribe_are_quieted(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        load = script.index("self._model = nemo_asr.models.ASRModel.from_pretrained")
        transcribe = script.index("result = self._call_transcribe(path)")
        self.assertLess(script.rfind("with _nemo_output_context():", 0, load), load)
        self.assertLess(script.rfind("with _nemo_output_context():", 0, transcribe), transcribe)

class PostprocessTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_POST_PROCESSOR", "VOICEPI_POST_MODE", "VOICEPI_POST_MODEL",
            "VOICEPI_POST_BASE_URL", "VOICEPI_POST_TIMEOUT_MS",
            "VOICEPI_POST_MAX_INPUT_CHARS", "VOICEPI_POST_MAX_OUTPUT_CHARS",
            "VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY", "OPENAI_API_KEY",
            "GROQ_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
        for n in ("vp_postprocess", "vp_config", "vp_privacy", "vp_external_api"):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k in self._old:
            os.environ.pop(k, None)
        for k, v in self._old.items():
            if v is not None:
                os.environ[k] = v
        for n in ("vp_postprocess", "vp_config", "vp_privacy", "vp_external_api"):
            sys.modules.pop(n, None)

    def test_default_ollama_model_literal_is_centralized(self):
        source = Path("vp_postprocess.py").read_text(encoding="utf-8")

        self.assertIn('DEFAULT_OLLAMA_POST_MODEL = "qwen2.5:3b"', source)
        self.assertEqual(source.count('"qwen2.5:3b"'), 1)

    def test_raw_mode_returns_text_unchanged(self):
        import vp_postprocess

        result = vp_postprocess.postprocess_text("keep this")

        self.assertEqual(result.text, "keep this")
        self.assertFalse(result.changed)
        self.assertEqual(result.provider, "none")
        self.assertEqual(result.mode, "raw")

    def test_postprocess_mode_prompts_cover_roadmap_modes(self):
        import vp_postprocess

        expectations = {
            "clean": "Clean punctuation",
            "prompt": "AI coding agent",
            "terminal": "Preserve commands",
            "slack": "Slack-style message",
            "email": "polished but faithful email",
            "bullets": "concise bullet points",
            "bullet-list": "concise bullet points",
        }
        for mode, phrase in expectations.items():
            with self.subTest(mode=mode):
                prompt = vp_postprocess.build_prompt("hello world", mode)
                self.assertIn(phrase, prompt)
                self.assertIn("Return only the rewritten text", prompt)

    def test_postprocess_accepts_bullet_list_alias(self):
        os.environ["VOICEPI_POST_PROCESSOR"] = "ollama"
        os.environ["VOICEPI_POST_MODE"] = "bullet-list"
        import vp_postprocess

        settings = vp_postprocess.load_postprocess_settings()

        self.assertEqual(settings.mode, "bullets")
        result = vp_postprocess.postprocess_text("fallback", vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="bullet-list",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        ))
        self.assertEqual(result.mode, "bullets")
        self.assertTrue(result.fallback)

    def test_clean_mode_uses_fake_ollama_server(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["payload"] = json.loads(body.decode("utf-8"))
                data = json.dumps({"response": "Hello, world."}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                # Silence the in-process HTTP server during this test.
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url=f"http://127.0.0.1:{server.server_port}",
        )
        result = vp_postprocess.postprocess_text("hello world", settings)

        self.assertEqual(result.text, "Hello, world.")
        self.assertTrue(result.changed)
        self.assertEqual(result.model, "qwen2.5:3b")
        self.assertEqual(calls["path"], "/api/generate")
        self.assertEqual(calls["payload"]["model"], "qwen2.5:3b")
        self.assertIn("Clean punctuation", calls["payload"]["prompt"])

    def test_openai_postprocessor_uses_fake_chat_server(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["auth"] = self.headers.get("Authorization")
                calls["payload"] = json.loads(body.decode("utf-8"))
                data = json.dumps({
                    "choices": [{
                        "message": {"content": "Cleaned text."}
                    }]
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                # Silence the in-process HTTP server during this test.
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url=f"http://127.0.0.1:{server.server_port}/v1",
            api_key="test-key",
        )
        result = vp_postprocess.postprocess_text("cleaned text", settings)

        self.assertEqual(result.text, "Cleaned text.")
        self.assertEqual(result.provider, "openai")
        self.assertEqual(calls["path"], "/v1/chat/completions")
        self.assertEqual(calls["auth"], "Bearer test-key")
        self.assertIn("Clean punctuation", calls["payload"]["messages"][1]["content"])

    def test_groq_postprocessor_defaults_to_groq_chat_model_and_key(self):
        os.environ["VOICEPI_POST_PROCESSOR"] = "groq"
        os.environ["VOICEPI_POST_BASE_URL"] = "http://localhost:11434"
        os.environ["VOICEPI_POST_MODEL"] = "qwen2.5:3b"
        os.environ["GROQ_API_KEY"] = "groq-test-key"
        import vp_postprocess

        settings = vp_postprocess.load_postprocess_settings()

        self.assertEqual(settings.processor, "groq")
        self.assertEqual(settings.base_url, "https://api.groq.com/openai/v1")
        self.assertEqual(settings.model, "llama-3.1-8b-instant")
        self.assertEqual(settings.api_key, "groq-test-key")

    def test_groq_postprocessor_uses_openai_compatible_chat_server(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["auth"] = self.headers.get("Authorization")
                calls["payload"] = json.loads(body.decode("utf-8"))
                data = json.dumps({
                    "choices": [{
                        "message": {"content": "Final pass text."}
                    }]
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                # Silence the in-process HTTP server during this test.
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="groq",
            mode="clean",
            model="llama-3.1-8b-instant",
            base_url=f"http://127.0.0.1:{server.server_port}/openai/v1",
            api_key="groq-test-key",
        )
        result = vp_postprocess.postprocess_text("final pass text", settings)

        self.assertEqual(result.text, "Final pass text.")
        self.assertEqual(result.provider, "groq")
        self.assertEqual(calls["path"], "/openai/v1/chat/completions")
        self.assertEqual(calls["auth"], "Bearer groq-test-key")
        self.assertEqual(calls["payload"]["model"], "llama-3.1-8b-instant")

    def test_openai_postprocessor_redacts_before_cloud_and_restores_output(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["payload"] = json.loads(body.decode("utf-8"))
                prompt = calls["payload"]["messages"][1]["content"]
                data = json.dumps({
                    "choices": [{
                        "message": {"content": "Contact [[WD_TERM_2]] at [[WD_EMAIL_1]]."}
                    }]
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                calls["prompt"] = prompt

            def log_message(self, *args):
                # Silence the in-process HTTP server during this test.
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url=f"http://127.0.0.1:{server.server_port}/v1",
            api_key="test-key",
            redact=True,
            redact_terms="Lars Andersen",
        )
        result = vp_postprocess.postprocess_text(
            "Contact Lars Andersen at lars@example.com.", settings)

        self.assertNotIn("Lars Andersen", calls["prompt"])
        self.assertNotIn("lars@example.com", calls["prompt"])
        self.assertIn("[[WD_TERM_2]]", calls["prompt"])
        self.assertIn("[[WD_EMAIL_1]]", calls["prompt"])
        self.assertEqual(result.text, "Contact Lars Andersen at lars@example.com.")
        self.assertTrue(result.redacted)
        self.assertTrue(result.redactions)
        self.assertTrue(all("value" not in item for item in result.redactions))

    def test_ollama_failure_falls_back_to_original_text(self):
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        result = vp_postprocess.postprocess_text("fallback text", settings)

        self.assertEqual(result.text, "fallback text")
        self.assertTrue(result.fallback)
        self.assertTrue(result.error)

    def test_local_only_blocks_remote_postprocess_url(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="https://example.com",
        )

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_postprocess.validate_postprocess_settings(settings)

    def test_local_only_blocks_openai_postprocessor_even_on_localhost(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            base_url="http://localhost:11434",
            api_key="test-key",
        )

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_postprocess.validate_postprocess_settings(settings)

    def test_local_only_allows_localhost_postprocess_url(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="http://localhost:11434",
        )

        vp_postprocess.validate_postprocess_settings(settings)

    def test_voice_pi_records_postprocess_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("postprocess_text(text", script)
        self.assertIn("dictionary_text=source_text", script)
        self.assertIn("post_processor=post_result.provider", script)
        self.assertIn("post_fallback=post_result.fallback", script)

    def test_voice_pi_logs_postprocess_status_for_every_utterance(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("[post] skipped", script)
        self.assertIn("[post] fallback after", script)
        self.assertIn("unchanged", script)
        self.assertIn("post_result.changed", script)

class FormatCommandTests(unittest.TestCase):
    def setUp(self):
        sys.modules.pop("vp_formatting", None)

    def test_format_commands_are_off_by_default(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands("write comma literally")

        self.assertFalse(result.enabled)
        self.assertEqual(result.text, "write comma literally")

    def test_english_format_commands_replace_whole_phrases(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "first item comma new line second item period", "en")

        self.assertTrue(result.enabled)
        self.assertTrue(result.changed)
        self.assertEqual(result.text, "first item,\nsecond item.")
        self.assertIn({"command": "new line", "replacement": "\n", "count": "1"}, result.applied)

    def test_danish_format_commands_replace_whole_phrases(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "første punkt komma ny linje andet punkt punktum", "da")

        self.assertEqual(result.text, "første punkt,\nandet punkt.")

    def test_format_commands_do_not_replace_inside_words(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "Common words and kommandolinje stay literal", "both")

        self.assertFalse(result.changed)
        self.assertEqual(result.text, "Common words and kommandolinje stay literal")

    def test_format_tidy_normalizes_spacing_without_regex_backtracking(self):
        import vp_formatting

        cleaned = vp_formatting._tidy(
            "first      ,second\n   third      -      fourth\n\n\n\nfifth")

        self.assertEqual(cleaned, "first, second\nthird - fourth\n\nfifth")

    def test_python_formatting_delegates_to_rust_helper_when_available(self):
        import subprocess
        import vp_formatting

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"],
            0,
            stdout=json.dumps({
                "text": "første,\nandet.",
                "enabled": True,
                "changed": True,
                "command_set": "da",
                "applied": [{"command": "komma", "replacement": ",", "count": 1}],
            }),
            stderr="",
        )

        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "whisper-dictate"}), \
                patch("vp_formatting.subprocess.run", return_value=completed) as run:
            result = vp_formatting.apply_format_commands("første komma ny linje andet punktum", "da")

        self.assertEqual(result.text, "første,\nandet.")
        self.assertEqual(result.applied[0]["count"], "1")
        self.assertEqual(run.call_args.args[0][:2], ["whisper-dictate", "format-text"])

    def test_python_formatting_falls_back_when_rust_helper_fails(self):
        import subprocess
        import vp_formatting

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"],
            1,
            stdout="",
            stderr="boom",
        )

        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "whisper-dictate"}), \
                patch("vp_formatting.subprocess.run", return_value=completed):
            result = vp_formatting.apply_format_commands("first comma", "en")

        self.assertEqual(result.text, "first,")

    def test_voice_pi_applies_formatting_before_injection_and_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        post_pos = script.index("def _postprocess_and_format")
        format_pos = script.index("format_result = apply_format_commands")
        inject_pos = script.index("self._inject(final_text)")
        metrics_pos = script.index("event = self._utterance_event(")
        self.assertLess(post_pos, format_pos)
        self.assertLess(format_pos, inject_pos)
        self.assertLess(inject_pos, metrics_pos)
