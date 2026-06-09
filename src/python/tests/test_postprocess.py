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

class PostprocessTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_POST_PROCESSOR", "VOICEPI_POST_MODE", "VOICEPI_POST_MODEL",
            "VOICEPI_POST_BASE_URL", "VOICEPI_POST_TIMEOUT_MS",
            "VOICEPI_POST_MAX_INPUT_CHARS", "VOICEPI_POST_MAX_OUTPUT_CHARS",
            "VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY", "OPENAI_API_KEY",
            "GROQ_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
        for n in ("vp_postprocess", "vp_config", "vp_external_api"):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k in self._old:
            os.environ.pop(k, None)
        for k, v in self._old.items():
            if v is not None:
                os.environ[k] = v
        for n in ("vp_postprocess", "vp_config", "vp_external_api"):
            sys.modules.pop(n, None)

    def test_default_ollama_model_literal_is_centralized(self):
        source = Path("src/python/whisper_dictate/vp_postprocess.py").read_text(encoding="utf-8")

        self.assertIn('DEFAULT_OLLAMA_POST_MODEL = "qwen2.5:3b"', source)
        self.assertEqual(source.count('"qwen2.5:3b"'), 1)

    def test_raw_mode_returns_text_unchanged(self):
        from whisper_dictate import vp_postprocess

        result = vp_postprocess.postprocess_text("keep this")

        self.assertEqual(result.text, "keep this")
        self.assertFalse(result.changed)
        self.assertEqual(result.provider, "none")
        self.assertEqual(result.mode, "raw")

    def test_postprocess_mode_prompts_cover_roadmap_modes(self):
        from whisper_dictate import vp_postprocess

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
                self.assertIn("Do not include the original text", prompt)
                if mode == "clean":
                    self.assertIn("Do not paraphrase", prompt)

    def test_postprocess_accepts_bullet_list_alias(self):
        os.environ["VOICEPI_POST_PROCESSOR"] = "ollama"
        os.environ["VOICEPI_POST_MODE"] = "bullet-list"
        from whisper_dictate import vp_postprocess

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

        from whisper_dictate import vp_postprocess

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

        from whisper_dictate import vp_postprocess

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

    def test_postprocessor_extracts_final_text_from_before_after_answer(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        source = "Hej, mit navn er Sara. Jeg er Lars' datter."
        final = "Hej, mit navn er Sara. Jeg er datter af Lars."

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                data = json.dumps({
                    "choices": [{
                        "message": {"content": f"{source}\n\nbecomes\n\n{final}"}
                    }]
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url=f"http://127.0.0.1:{server.server_port}/v1",
            api_key="test-key",
        )
        result = vp_postprocess.postprocess_text(source, settings)

        self.assertEqual(result.text, final)
        self.assertNotIn("becomes", result.text)

    def test_groq_postprocessor_defaults_to_groq_chat_model_and_key(self):
        os.environ["VOICEPI_POST_PROCESSOR"] = "groq"
        os.environ["VOICEPI_POST_BASE_URL"] = "http://localhost:11434"
        os.environ["VOICEPI_POST_MODEL"] = "qwen2.5:3b"
        os.environ["GROQ_API_KEY"] = "groq-test-key"
        from whisper_dictate import vp_postprocess

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

        from whisper_dictate import vp_postprocess

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
                payload = json.loads(body.decode("utf-8"))
                # Record everything the test asserts on BEFORE sending the
                # response. Otherwise the client can receive the reply and return
                # from postprocess_text while this server thread hasn't reached the
                # recording lines yet — a race that made `calls["prompt"]` flaky.
                calls["payload"] = payload
                calls["prompt"] = payload["messages"][1]["content"]
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

            def log_message(self, *args):
                # Silence the in-process HTTP server during this test.
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url=f"http://127.0.0.1:{server.server_port}/v1",
            api_key="test-key",
            redact=True,
            redact_terms="Lars Andersen",
        )
        redaction = {
            "text": "Contact [[WD_TERM_2]] at [[WD_EMAIL_1]].",
            "redactions": [
                {
                    "placeholder": "[[WD_EMAIL_1]]",
                    "value": "lars@example.com",
                    "kind": "email",
                },
                {
                    "placeholder": "[[WD_TERM_2]]",
                    "value": "Lars Andersen",
                    "kind": "term",
                },
            ],
        }
        def rust_json(command, *_args, **_kwargs):
            if command == "privacy":
                return {"ok": True}
            return redaction

        with patch("whisper_dictate.vp_postprocess._rust_json", side_effect=rust_json):
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
        from whisper_dictate import vp_postprocess

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
        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="https://example.com",
        )

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_postprocess.validate_postprocess_settings(settings)

    def test_local_only_blocks_openai_postprocessor_even_on_localhost(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        from whisper_dictate import vp_postprocess

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
        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="http://localhost:11434",
        )

        vp_postprocess.validate_postprocess_settings(settings)

    def test_runtime_records_postprocess_metrics(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("postprocess_text(text", script)
        self.assertIn("dictionary_text=source_text", script)
        self.assertIn('"post_processor": post_result.provider', script)
        self.assertIn('"post_fallback": post_result.fallback', script)

    def test_runtime_logs_postprocess_status_for_every_utterance(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("[post] skipped", script)
        self.assertIn("[post] fallback after", script)
        self.assertIn("unchanged", script)
        self.assertIn("post_result.changed", script)

class FormatCommandTests(unittest.TestCase):
    def setUp(self):
        sys.modules.pop("whisper_dictate.runtime", None)

    def test_format_commands_are_off_by_default(self):
        from whisper_dictate import runtime

        result = runtime.apply_format_commands("write comma literally")

        self.assertFalse(result.enabled)
        self.assertEqual(result.text, "write comma literally")

    def test_format_commands_require_rust_helper_when_enabled(self):
        from whisper_dictate import runtime

        with patch.dict(os.environ, {}, clear=True), \
                self.assertRaisesRegex(RuntimeError, "Rust format-text helper"):
            runtime.apply_format_commands("first comma", "en")

    def test_python_formatting_reports_rust_helper_failure(self):
        import subprocess
        from whisper_dictate import runtime

        completed = subprocess.CompletedProcess(
            ["whisper-dictate"],
            1,
            stdout="",
            stderr="boom",
        )

        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "whisper-dictate"}), \
                patch("whisper_dictate.runtime.subprocess.run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "boom"):
                runtime.apply_format_commands("first comma", "en")

    def test_runtime_applies_formatting_before_injection_and_metrics(self):
        with open("src/python/whisper_dictate/vp_dictate.py", encoding="utf-8") as f:
            script = f.read()

        post_pos = script.index("def _postprocess_and_format")
        format_pos = script.index("format_result = apply_format_commands")
        inject_pos = script.index("self._inject(final_text)")
        metrics_pos = script.index("event = self._utterance_event(")
        self.assertLess(post_pos, format_pos)
        self.assertLess(format_pos, inject_pos)
        self.assertLess(inject_pos, metrics_pos)
