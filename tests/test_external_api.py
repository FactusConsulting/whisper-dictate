from tests.test_helpers import (
    _env,
    dataclasses,
    io,
    json,
    os,
    patch,
    real_numpy,
    subprocess,
    sys,
    unittest,
)

class ExternalApiTests(unittest.TestCase):
    def test_external_api_import_path_does_not_require_numpy_until_transcription(self):
        completed = subprocess.run(
            [sys.executable, "-S", "-c",
             "import vp_external_api, vp_postprocess; "
             "assert vp_external_api.DEFAULT_OPENAI_BASE_URL; "
             "assert 'openai' in vp_postprocess.VALID_PROCESSORS"],
            cwd=os.getcwd(),
            capture_output=True,
            text=True,
            timeout=10,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_external_stt_maps_local_whisper_default_to_openai_model(self):
        with _env(VOICEPI_MODEL="large-v3-turbo", VOICEPI_STT_API_KEY="test-key"):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3-turbo")

        self.assertEqual(settings.model, "gpt-4o-mini-transcribe")

    def test_external_stt_configured_model_takes_precedence(self):
        with _env(VOICEPI_STT_MODEL="gpt-4o-transcribe", VOICEPI_STT_API_KEY="test-key"):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.model, "gpt-4o-transcribe")

    def test_groq_base_url_accepts_groq_api_key_alias(self):
        with _env(
            VOICEPI_STT_API_KEY=None,
            OPENAI_API_KEY=None,
            GROQ_API_KEY="groq-key",
            VOICEPI_STT_BASE_URL="https://api.groq.com/openai/v1",
            VOICEPI_STT_MODEL="whisper-large-v3-turbo",
        ):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.base_url, "https://api.groq.com/openai/v1")
        self.assertEqual(settings.model, "whisper-large-v3-turbo")
        self.assertEqual(settings.api_key, "groq-key")

    def test_external_api_adds_default_user_agent_for_groq_gateway(self):
        sys.modules.pop("vp_external_api", None)
        import vp_external_api

        headers = vp_external_api.default_headers({"Authorization": "Bearer test"})

        self.assertIn("User-Agent", headers)
        self.assertIn("whisper-dictate", headers["User-Agent"])
        self.assertEqual(headers["Authorization"], "Bearer test")

    def test_external_api_preserves_explicit_user_agent(self):
        sys.modules.pop("vp_external_api", None)
        import vp_external_api

        headers = vp_external_api.default_headers({"User-Agent": "custom-client"})

        self.assertEqual(headers["User-Agent"], "custom-client")

    def test_groq_transcription_prompt_is_capped_to_provider_limit(self):
        sys.modules.pop("vp_external_api", None)
        import vp_external_api

        prompt = "x" * 911
        capped = vp_external_api._cap_transcription_prompt(
            prompt,
            base_url="https://api.groq.com/openai/v1",
        )

        self.assertEqual(len(capped), 896)

    def test_non_groq_transcription_prompt_is_not_capped(self):
        sys.modules.pop("vp_external_api", None)
        import vp_external_api

        prompt = "x" * 911
        capped = vp_external_api._cap_transcription_prompt(
            prompt,
            base_url="https://api.openai.com/v1",
        )

        self.assertEqual(capped, prompt)

    def test_non_groq_base_url_does_not_use_groq_api_key_alias(self):
        with _env(
            VOICEPI_STT_API_KEY=None,
            OPENAI_API_KEY=None,
            GROQ_API_KEY="groq-key",
            VOICEPI_STT_BASE_URL="https://api.example.test/v1",
            VOICEPI_STT_MODEL="custom-transcribe",
        ):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.api_key, "")

    def test_external_transcription_posts_multipart_audio(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer
        sys.modules.pop("vp_external_api", None)
        try:
            np = real_numpy()
        except ImportError:
            self.skipTest("real numpy unavailable")
        sys.modules["numpy"] = np

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["auth"] = self.headers.get("Authorization")
                calls["user_agent"] = self.headers.get("User-Agent")
                calls["content_type"] = self.headers.get("Content-Type")
                calls["body"] = body
                data = json.dumps({"text": "Hello Codex", "language": "en"}).encode("utf-8")
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

        with _env(
            VOICEPI_STT_API_KEY="test-key",
            VOICEPI_STT_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
        ):
            import vp_external_api

            model = vp_external_api.ExternalTranscriptionModel("gpt-4o-mini-transcribe")
            segments, info = model.transcribe(np.zeros(1600, dtype=np.float32), language="en")

        self.assertEqual(calls["path"], "/v1/audio/transcriptions")
        self.assertEqual(calls["auth"], "Bearer test-key")
        self.assertIn("whisper-dictate", calls["user_agent"])
        self.assertIn("multipart/form-data", calls["content_type"])
        self.assertIn(b'gpt-4o-mini-transcribe', calls["body"])
        self.assertIn(b"audio.wav", calls["body"])
        self.assertEqual(segments[0].text.strip(), "Hello Codex")
        self.assertEqual(info.language, "en")

    def test_external_transcription_caps_groq_prompt_in_multipart_audio(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer
        sys.modules.pop("vp_external_api", None)
        try:
            np = real_numpy()
        except ImportError:
            self.skipTest("real numpy unavailable")
        sys.modules["numpy"] = np

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                calls["body"] = self.rfile.read(int(self.headers["Content-Length"]))
                data = json.dumps({"text": "ok"}).encode("utf-8")
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

        with _env(
            VOICEPI_STT_API_KEY="test-key",
            VOICEPI_STT_BASE_URL=f"http://127.0.0.1:{server.server_port}/openai/v1",
        ):
            import vp_external_api

            model = vp_external_api.ExternalTranscriptionModel("whisper-large-v3-turbo")
            model.settings = dataclasses.replace(
                model.settings,
                base_url="https://api.groq.com/openai/v1",
            )
            with patch.object(vp_external_api, "_request_json", return_value={"text": "ok"}) as request:
                model.transcribe(np.zeros(1600, dtype=np.float32), initial_prompt="x" * 911)

        body = request.call_args.kwargs["data"]
        self.assertIn(b"x" * 896, body)
        self.assertNotIn(b"x" * 897, body)

    def test_external_api_http_error_includes_provider_message(self):
        import urllib.error
        sys.modules.pop("vp_external_api", None)
        import vp_external_api

        body = io.BytesIO(json.dumps({
            "error": {"message": "model access denied for this API key"}
        }).encode("utf-8"))
        error = urllib.error.HTTPError(
            "https://api.groq.com/openai/v1/audio/transcriptions",
            403,
            "Forbidden",
            hdrs=None,
            fp=body,
        )

        with patch("urllib.request.urlopen", side_effect=error):
            with self.assertRaisesRegex(RuntimeError, "model access denied"):
                vp_external_api._request_json(
                    "https://api.groq.com/openai/v1/audio/transcriptions",
                    data=b"body",
                    headers={},
                    timeout_ms=1000,
                )

class RedactionTests(unittest.TestCase):
    def test_redacts_email_phone_tokens_and_custom_terms_without_public_values(self):
        import vp_redaction

        result = vp_redaction.redact_text(
            "Mail lars@example.com or call +45 12 34 56 78 with sk-testtoken123456789",
            terms=["Lars Andersen"],
        )

        self.assertNotIn("lars@example.com", result.text)
        self.assertNotIn("+45 12 34 56 78", result.text)
        self.assertIn("[[WD_EMAIL_1]]", result.text)
        self.assertIn("[[WD_PHONE_", result.text)
        restored = result.restore(result.text)
        self.assertIn("lars@example.com", restored)
        summary = result.public_summary()
        self.assertTrue(all("value" not in item for item in summary))
        self.assertTrue(any(item["kind"] == "email" for item in summary))
