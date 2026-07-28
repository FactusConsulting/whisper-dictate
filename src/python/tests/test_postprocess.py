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
            "VOICEPI_POST_API_KEY", "VOICEPI_POST_API_KEY_ENDPOINT",
            "VOICEPI_STT_API_KEY", "OPENAI_API_KEY",
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

    def test_load_postprocess_settings_reads_config_once(self):
        from whisper_dictate import vp_postprocess
        from whisper_dictate.vp_config import ConfigSnapshot

        calls = []
        data = {
            "post_processor": "groq",
            "post_mode": "clean",
            "post_timeout_ms": "1234",
            "post_max_input_chars": "2345",
            "post_max_output_chars": "3456",
        }

        def fake_snapshot():
            calls.append(1)
            return ConfigSnapshot(data)

        with patch.object(vp_postprocess, "config_snapshot", fake_snapshot):
            settings = vp_postprocess.load_postprocess_settings()

        self.assertEqual(settings.processor, "groq")
        self.assertEqual(settings.timeout_ms, 1234)
        self.assertEqual(calls, [1])

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

    def test_effective_timeout_scales_with_length_and_clamps(self):
        from whisper_dictate import vp_postprocess

        base = 4000
        # 0 chars -> exactly the base (acts as floor).
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, 0), 4000)
        # 60 chars -> 4000 + 60*20 = 5200.
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, 60), 5200)
        # 444 chars (the bug repro) -> 4000 + 444*20 = 12880.
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, 444), 12880)
        # 1000 chars -> 24000, but clamped to the 30000 ceiling? 4000+20000=24000
        # is below ceiling; 1300 chars -> 30000, anything beyond stays at ceiling.
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, 1300), 30000)
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, 100000), 30000)
        # The base is the floor: negative/zero length never drops below base.
        self.assertEqual(vp_postprocess.effective_timeout_ms(base, -5), 4000)
        self.assertEqual(vp_postprocess.PER_CHAR_MS, 20)
        self.assertEqual(vp_postprocess.CEILING_MS, 30000)

    def test_cleanup_call_gets_length_scaled_timeout(self):
        from whisper_dictate import vp_postprocess

        captured = {}

        def fake_chat(*, base_url, api_key, model, prompt, timeout_ms):
            captured.setdefault("timeouts", []).append(timeout_ms)
            return "cleaned", 5

        def rust_json(command, *_a, **_k):
            return {"ok": True} if command == "privacy" else None

        def run(text):
            settings = vp_postprocess.PostprocessSettings(
                processor="groq",
                mode="clean",
                model="llama-3.1-8b-instant",
                base_url="https://api.groq.com/openai/v1",
                api_key="k",
                timeout_ms=4000,
            )
            with patch("whisper_dictate.vp_postprocess.openai_chat_completion",
                       side_effect=fake_chat), \
                    patch("whisper_dictate.vp_postprocess._rust_json",
                          side_effect=rust_json):
                vp_postprocess.postprocess_text(text, settings)

        run("x" * 10)
        run("x" * 500)
        short, long = captured["timeouts"]
        # Floor at base for tiny input, and the budget grows with length.
        self.assertEqual(short, 4000 + 10 * vp_postprocess.PER_CHAR_MS)
        self.assertEqual(long, 4000 + 500 * vp_postprocess.PER_CHAR_MS)
        self.assertGreater(long, short)

    def test_ollama_cleanup_call_gets_length_scaled_timeout(self):
        from whisper_dictate import vp_postprocess

        captured = {}

        def fake_urlopen(req, timeout=None):
            captured.setdefault("timeouts", []).append(timeout)
            import io
            import json as _json
            data = _json.dumps({"response": "cleaned"}).encode("utf-8")
            resp = io.BytesIO(data)
            resp.__enter__ = lambda s: s
            resp.__exit__ = lambda s, *a: None
            resp.read = lambda: data
            return resp

        def run(text):
            settings = vp_postprocess.PostprocessSettings(
                processor="ollama",
                mode="clean",
                model="qwen2.5:3b",
                base_url="http://127.0.0.1:11434",
                timeout_ms=4000,
            )
            with patch("whisper_dictate.vp_postprocess.urllib.request.urlopen",
                       side_effect=fake_urlopen):
                vp_postprocess.postprocess_text(text, settings)

        run("x" * 10)
        run("x" * 500)
        short_s, long_s = captured["timeouts"]
        # Convert seconds back to ms for comparison with scaling formula.
        short_ms = round(short_s * 1000)
        long_ms = round(long_s * 1000)
        self.assertEqual(short_ms, 4000 + 10 * vp_postprocess.PER_CHAR_MS)
        self.assertEqual(long_ms, 4000 + 500 * vp_postprocess.PER_CHAR_MS)
        self.assertGreater(long_ms, short_ms)

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

    def test_endpoint_marker_mismatch_rejects_groq_key_going_to_openai(self):
        # Codex P1 #642 exact leak scenario: launcher injected a Groq key +
        # marker, live change moved base_url to OpenAI, must NOT send the
        # Groq key across the wire.
        from whisper_dictate import vp_postprocess

        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="https://api.openai.com/v1",
            marker="https://api.groq.com/openai/v1",
        )
        self.assertIn("refusing to send", err)
        self.assertIn("groq", err)
        self.assertIn("openai", err)

    def test_endpoint_marker_mismatch_rejects_groq_key_going_to_custom_host(self):
        # The most dangerous case: a live change to a self-hosted URL. The
        # stored Groq key must never be exfiltrated to an arbitrary host.
        from whisper_dictate import vp_postprocess

        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="https://llm.internal.example/v1",
            marker="https://api.groq.com/openai/v1",
        )
        self.assertIn("refusing to send", err)
        self.assertIn("custom", err)

    def test_endpoint_marker_mismatch_allows_same_provider_url_edit(self):
        # Groq default -> Groq beta URL is legitimate: same provider, same
        # key. Classification is by HOST suffix, so both sides map to Groq.
        from whisper_dictate import vp_postprocess

        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://api.groq.com/openai/v1",
                marker="https://api.groq.com/openai/v1",
            ),
            "",
        )
        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://api.groq.com/beta/v1",
                marker="https://api.groq.com/openai/v1",
            ),
            "",
        )

    def test_endpoint_marker_rejects_scheme_downgrade_to_http(self):
        # Codex P1 #666 #3 (`PRRT_kwDOSfNjQs6UXpn3`) Python parity: both
        # Rust and Python HTTP paths attach the Bearer to the initial
        # unencrypted request, so an https-marker -> http-base downgrade
        # must be rejected even when the provider still matches.
        from whisper_dictate import vp_postprocess

        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="http://api.groq.com/openai/v1",
            marker="https://api.groq.com/openai/v1",
        )
        self.assertIn("plaintext http", err)
        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="http://api.openai.com/v1",
            marker="https://api.openai.com/v1",
        )
        self.assertIn("plaintext http", err)
        # http marker -> https base is a legitimate upgrade -- allowed.
        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://api.groq.com/openai/v1",
                marker="http://api.groq.com/openai/v1",
            ),
            "",
        )

    def test_endpoint_marker_rejects_custom_origin_change(self):
        # Codex P1 #666 #4 (`PRRT_kwDOSfNjQs6UXpnz`) Python parity: two
        # different custom hosts share the "custom" provider label, so a
        # live change between self-hosted origins must be rejected on an
        # exact scheme+host+port match rather than the provider label.
        from whisper_dictate import vp_postprocess

        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="https://llm-b.example/v1",
            marker="https://llm-a.example/v1",
        )
        self.assertIn("different self-hosted origin", err)
        # Different port on the same host is a different origin.
        err = vp_postprocess.endpoint_marker_mismatch(
            base_url="https://llm-a.example:8081/v1",
            marker="https://llm-a.example:8080/v1",
        )
        self.assertIn("different self-hosted origin", err)
        # Same origin still passes.
        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://llm-a.example/v1",
                marker="https://llm-a.example/v1",
            ),
            "",
        )
        # Same-origin with a different path -- still fine (check is on
        # origin, not full URL).
        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://llm-a.example/other/path",
                marker="https://llm-a.example/v1",
            ),
            "",
        )

    def test_endpoint_marker_absent_is_backward_compatible(self):
        # A user who exports their own VOICEPI_POST_API_KEY without the
        # launcher-side marker must never be blocked.
        from whisper_dictate import vp_postprocess

        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://api.openai.com/v1", marker=""
            ),
            "",
        )
        self.assertEqual(
            vp_postprocess.endpoint_marker_mismatch(
                base_url="https://llm.internal.example/v1", marker="  "
            ),
            "",
        )

    def test_endpoint_marker_classification_is_host_not_substring(self):
        # `_endpoint_provider` must classify by HOST, not `contains` --
        # otherwise `https://api.groq.com@evil.example/v1` (host =
        # evil.example) and `https://groq.com.attacker.example/v1` (suffix
        # trap) would falsely classify as Groq and get the stored key.
        from whisper_dictate import vp_postprocess

        self.assertEqual(
            vp_postprocess._endpoint_provider("https://api.groq.com@evil.example/v1"),
            "custom",
        )
        self.assertEqual(
            vp_postprocess._endpoint_provider("https://groq.com.attacker.example/v1"),
            "custom",
        )
        self.assertEqual(
            vp_postprocess._endpoint_provider("https://api.groq.com/openai/v1"),
            "groq",
        )
        self.assertEqual(
            vp_postprocess._endpoint_provider("https://api.openai.com/v1"),
            "openai",
        )

    def test_postprocess_text_refuses_send_when_endpoint_moved_after_worker_spawn(self):
        # End-to-end shape of the leak: launcher stamped `groq` marker + key,
        # user changed post_processor / base_url live to OpenAI. The worker
        # sees the new URL AND still holds the Groq key from its env. The
        # pipeline must return a fallback WITHOUT calling openai_chat_completion.
        from whisper_dictate import vp_postprocess

        def blow_up(**kw):  # would issue the leaking request
            raise AssertionError(
                f"openai_chat_completion must not be called with a stale key: {kw}"
            )

        def rust_json(command, *_a, **_k):
            return {"ok": True} if command == "privacy" else None

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url="https://api.openai.com/v1",
            api_key="stolen-groq-key",
            api_key_endpoint="https://api.groq.com/openai/v1",
        )
        with patch(
            "whisper_dictate.vp_postprocess.openai_chat_completion", side_effect=blow_up
        ), patch("whisper_dictate.vp_postprocess._rust_json", side_effect=rust_json), patch(
            "whisper_dictate.vp_postprocess._rust_postprocess_text", return_value=None
        ):
            result = vp_postprocess.postprocess_text("please clean this", settings)

        self.assertTrue(result.fallback)
        self.assertIn("refusing to send", result.error)
        # No dictation is dropped: caller sees the input verbatim.
        self.assertEqual(result.text, "please clean this")

    def test_postprocess_text_same_provider_call_still_reaches_the_backend(self):
        # Sanity: the marker only blocks CROSS-provider calls. A same-provider
        # call (both Groq) must still dispatch normally.
        from whisper_dictate import vp_postprocess

        called = {"count": 0}

        def fake_chat(*, base_url, api_key, model, prompt, timeout_ms):
            called["count"] += 1
            return "cleaned", 5

        def rust_json(command, *_a, **_k):
            return {"ok": True} if command == "privacy" else None

        settings = vp_postprocess.PostprocessSettings(
            processor="groq",
            mode="clean",
            model="llama-3.1-8b-instant",
            base_url="https://api.groq.com/openai/v1",
            api_key="groq-key",
            api_key_endpoint="https://api.groq.com/openai/v1",
        )
        with patch(
            "whisper_dictate.vp_postprocess.openai_chat_completion", side_effect=fake_chat
        ), patch("whisper_dictate.vp_postprocess._rust_json", side_effect=rust_json), patch(
            "whisper_dictate.vp_postprocess._rust_postprocess_text", return_value=None
        ):
            result = vp_postprocess.postprocess_text("hello", settings)

        self.assertEqual(called["count"], 1)
        self.assertEqual(result.text, "cleaned")
        self.assertFalse(result.fallback)

    def test_load_postprocess_settings_reads_endpoint_marker(self):
        # The marker flows from env -> `PostprocessSettings.api_key_endpoint`
        # via `load_postprocess_settings` so the downstream check sees it.
        # FormatCommandTests has no env-cleanup setUp, so scope env writes
        # with `patch.dict` and force a fresh vp_config import (module-level
        # `apply_config_to_environ` cache).
        with patch.dict(
            os.environ,
            {
                "VOICEPI_POST_PROCESSOR": "groq",
                "VOICEPI_POST_API_KEY": "groq-key",
                "VOICEPI_POST_API_KEY_ENDPOINT": "https://api.groq.com/openai/v1",
            },
        ):
            for n in ("vp_postprocess", "vp_config"):
                sys.modules.pop(n, None)
            from whisper_dictate import vp_postprocess

            settings = vp_postprocess.load_postprocess_settings()

            self.assertEqual(settings.api_key, "groq-key")
            self.assertEqual(settings.api_key_endpoint, "https://api.groq.com/openai/v1")

    def test_rust_postprocess_envelope_forwards_endpoint_marker(self):
        # The shell-out to `whisper-dictate postprocess` must include the
        # marker so the Rust `postprocess` verb enforces the same rule for
        # cross-provider live changes.
        from whisper_dictate import vp_postprocess

        captured = {}

        class FakeCompleted:
            returncode = 0
            stdout = json.dumps({
                "text": "ok",
                "raw_text": "ok",
                "changed": False,
                "provider": "groq",
                "mode": "clean",
                "model": "llama-3.1-8b-instant",
                "latency_ms": 0,
                "fallback": False,
                "error": "",
                "redacted": False,
                "redactions": [],
            })
            stderr = ""

        def fake_run(cmd, **kwargs):
            captured["input"] = json.loads(kwargs["input"])
            return FakeCompleted()

        settings = vp_postprocess.PostprocessSettings(
            processor="groq",
            mode="clean",
            model="llama-3.1-8b-instant",
            base_url="https://api.groq.com/openai/v1",
            api_key="groq-key",
            api_key_endpoint="https://api.groq.com/openai/v1",
        )
        with patch.dict(os.environ, {"VOICEPI_RUST_INJECTOR": "whisper-dictate"}), \
                patch("whisper_dictate.vp_postprocess.helper_path", return_value="whisper-dictate"), \
                patch("whisper_dictate.vp_postprocess.subprocess.run", side_effect=fake_run):
            vp_postprocess._rust_postprocess_text("hello", settings)

        self.assertEqual(
            captured["input"]["settings"]["api_key_endpoint"],
            "https://api.groq.com/openai/v1",
        )

    def test_regression_p1_642_stale_groq_key_not_sent_after_live_endpoint_change(self):
        # Codex P1 #642 regression pin (safety-net memory `tests-as-safety-net.md`).
        #
        # Un-fixed code path: `attach_cloud_api_keys` injects
        # VOICEPI_POST_API_KEY into the worker env for a Groq resolution, the
        # worker later reloads a different `post_base_url` from live config,
        # and `openai_chat_completion` sends the SAME Groq bearer to the new
        # host -- exactly the leak the finding describes.
        #
        # This test exercises the ENTIRE seam without referencing the new
        # `api_key_endpoint` FIELD directly, so it would run under un-fixed
        # code and observe the leak: the outgoing Bearer would carry the
        # Groq key.
        #
        # * On un-fixed code (no marker plumbing):
        #     - `load_postprocess_settings` reads `api_key=groq-key`, has no
        #       marker awareness, and `postprocess_text` calls
        #       `openai_chat_completion` with the Groq key against the new
        #       custom URL. captured[0]['api_key'] == 'groq-key' -> ASSERT
        #       FAILS -> regression caught.
        # * On fixed code (marker + refuse):
        #     - `load_postprocess_settings` also reads
        #       VOICEPI_POST_API_KEY_ENDPOINT into `settings.api_key_endpoint`.
        #     - After the live change, `postprocess_text` classifies the new
        #       base_url (Custom) vs the marker (Groq) and REFUSES to call
        #       `openai_chat_completion`. captured stays empty -> assert
        #       passes.
        import dataclasses

        # Simulate what `attach_cloud_api_keys` injects into the worker env
        # when a user has saved a Groq credential and configured a Groq
        # post-processor with the default URL. FormatCommandTests has no
        # env-cleanup setUp, so scope env writes with `addCleanup`.
        # The marker (VOICEPI_POST_API_KEY_ENDPOINT) is what the launcher
        # stamps; un-fixed code silently ignores it, fixed code plumbs it
        # into `settings.api_key_endpoint` and refuses cross-provider sends.
        launcher_env = {
            "VOICEPI_POST_PROCESSOR": "groq",
            "VOICEPI_POST_MODE": "clean",
            "VOICEPI_POST_BASE_URL": "https://api.groq.com/openai/v1",
            "VOICEPI_POST_API_KEY": "groq-secret-key",
            "VOICEPI_POST_API_KEY_ENDPOINT": "https://api.groq.com/openai/v1",
        }
        env_ctx = patch.dict(os.environ, launcher_env)
        env_ctx.start()
        self.addCleanup(env_ctx.stop)
        for n in ("vp_postprocess", "vp_config"):
            sys.modules.pop(n, None)
        self.addCleanup(lambda: [sys.modules.pop(n, None) for n in ("vp_postprocess", "vp_config")])

        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.load_postprocess_settings()
        # Sanity: worker really has the Groq key after "spawn".
        self.assertEqual(settings.api_key, "groq-secret-key")
        self.assertEqual(settings.processor, "groq")

        # LIVE CHANGE: user edits `post_base_url` in Settings to a
        # self-hosted / arbitrary URL. In the running worker this shows up
        # as a fresh settings snapshot; we model that by rebuilding the
        # settings dataclass with the swapped base_url. Uses
        # `dataclasses.replace` so the test compiles under both un-fixed
        # (only original fields) and fixed (adds `api_key_endpoint`) code.
        live_changed = dataclasses.replace(
            settings, base_url="https://llm.internal.example/v1"
        )

        captured = []

        def spy_chat(**kw):
            captured.append(kw)
            # Return a plausible response so the pipeline finishes cleanly.
            return "unused", 5

        def rust_json(command, *_a, **_k):
            return {"ok": True} if command == "privacy" else None

        with patch(
            "whisper_dictate.vp_postprocess.openai_chat_completion", side_effect=spy_chat
        ), patch(
            "whisper_dictate.vp_postprocess._rust_json", side_effect=rust_json
        ), patch(
            "whisper_dictate.vp_postprocess._rust_postprocess_text", return_value=None
        ):
            vp_postprocess.postprocess_text("please clean this", live_changed)

        # The whole point: whatever the pipeline does, it must NOT have sent
        # the Groq bearer token to a different-provider host. Two acceptable
        # shapes for a fix: (A) the call was skipped entirely; (B) the key
        # was re-resolved to something else. Either passes; the un-fixed
        # `Authorization: Bearer groq-secret-key` fails.
        leaks = [c for c in captured if c.get("api_key") == "groq-secret-key"]
        self.assertEqual(
            leaks,
            [],
            "SECURITY REGRESSION (Codex P1 #642): launcher-injected Groq key "
            "was sent to a different-provider endpoint after a live "
            f"post_base_url change. Captured calls: {captured}",
        )

    def test_regression_p1_642_stale_groq_key_not_sent_when_processor_flipped_to_openai(self):
        # Companion to the base_url-change regression above: same leak
        # scenario but exercised via a `post_processor` flip (Groq -> OpenAI).
        # A `_normalized_base_url` substitution then lands the request on
        # `api.openai.com`, where the stored Groq key would be an exact
        # cross-provider leak. Same fail-on-unfixed / pass-on-fixed shape.
        import dataclasses

        launcher_env = {
            "VOICEPI_POST_PROCESSOR": "groq",
            "VOICEPI_POST_MODE": "clean",
            "VOICEPI_POST_BASE_URL": "https://api.groq.com/openai/v1",
            "VOICEPI_POST_API_KEY": "groq-secret-key",
            "VOICEPI_POST_API_KEY_ENDPOINT": "https://api.groq.com/openai/v1",
        }
        env_ctx = patch.dict(os.environ, launcher_env)
        env_ctx.start()
        self.addCleanup(env_ctx.stop)
        for n in ("vp_postprocess", "vp_config"):
            sys.modules.pop(n, None)
        self.addCleanup(lambda: [sys.modules.pop(n, None) for n in ("vp_postprocess", "vp_config")])

        from whisper_dictate import vp_postprocess

        settings = vp_postprocess.load_postprocess_settings()
        # LIVE CHANGE: processor swapped, and the settings a live reload
        # would produce include the OpenAI-default URL (the normaliser
        # substitutes it because the saved base_url matched the previous
        # processor's default).
        live_changed = dataclasses.replace(
            settings,
            processor="openai",
            base_url="https://api.openai.com/v1",
        )

        captured = []

        def spy_chat(**kw):
            captured.append(kw)
            return "unused", 5

        def rust_json(command, *_a, **_k):
            return {"ok": True} if command == "privacy" else None

        with patch(
            "whisper_dictate.vp_postprocess.openai_chat_completion", side_effect=spy_chat
        ), patch(
            "whisper_dictate.vp_postprocess._rust_json", side_effect=rust_json
        ), patch(
            "whisper_dictate.vp_postprocess._rust_postprocess_text", return_value=None
        ):
            vp_postprocess.postprocess_text("please clean this", live_changed)

        leaks = [c for c in captured if c.get("api_key") == "groq-secret-key"]
        self.assertEqual(
            leaks,
            [],
            "SECURITY REGRESSION (Codex P1 #642): launcher-injected Groq key "
            "was sent to OpenAI after a live post_processor flip. Captured "
            f"calls: {captured}",
        )

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
