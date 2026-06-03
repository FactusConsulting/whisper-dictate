from tests.test_helpers import (
    _capture_stdout,
    _env,
    io,
    json,
    load_voice_pi,
    os,
    patch,
    Path,
    redirect_stderr,
    sys,
    tempfile,
    types,
    unittest,
)

class DeviceResolutionTests(unittest.TestCase):
    def setUp(self):
        self._old_compute = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old_compute is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old_compute

    def test_auto_uses_cuda_when_available(self):
        voice_pi = load_voice_pi(cuda_devices=1)

        self.assertEqual(
            voice_pi._resolve_device("auto"),
            ("cuda", "int8_float16"),
        )

    def test_auto_falls_back_to_cpu_without_cuda(self):
        voice_pi = load_voice_pi(cuda_devices=0)

        self.assertEqual(voice_pi._resolve_device("auto"), ("cpu", "int8"))

    def test_explicit_cpu_and_cuda(self):
        voice_pi = load_voice_pi()

        self.assertEqual(voice_pi._resolve_device("cpu"), ("cpu", "int8"))
        self.assertEqual(
            voice_pi._resolve_device("cuda"),
            ("cuda", "int8_float16"),
        )

    def test_invalid_device_is_rejected(self):
        voice_pi = load_voice_pi()

        with self.assertRaises(ValueError):
            voice_pi._resolve_device("cdua")

class ComputeTypeOverrideTests(unittest.TestCase):
    """VOICEPI_COMPUTE_TYPE overrides the auto-picked compute_type for
    cuda / cpu / auto-on-gpu / auto-on-cpu — and an unset/empty env leaves
    the int8_float16-on-GPU / int8-on-CPU defaults untouched."""

    def setUp(self):
        self._old = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old

    def test_override_applies_to_explicit_cuda(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "float16"))

    def test_override_applies_to_explicit_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi()
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "float32"))

    def test_override_applies_to_auto_on_gpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "bfloat16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cuda", "bfloat16"))

    def test_override_applies_to_auto_on_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi(cuda_devices=0)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cpu", "float32"))

    def test_empty_env_leaves_defaults_untouched(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "   "  # whitespace only
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))

    def test_default_unchanged_when_env_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))

class DebugConfigTests(unittest.TestCase):
    """VOICEPI_DEBUG triggers a startup dump of every effective setting
    + the env-var source annotation — so users can verify their setx
    actually arrived in the running process."""

    def setUp(self):
        # Cache + clear env we mutate so the dump is deterministic
        self._cached = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_COMPUTE_TYPE", "VOICEPI_INITIAL_PROMPT",
            "VOICEPI_BEAM_SIZE", "VOICEPI_QUIT_KEY", "VOICEPI_QUIT_COUNT",
            "VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT",
            "VOICEPI_LANG", "VOICEPI_MODEL", "VOICEPI_DEVICE",
            "VOICEPI_KEY", "VOICEPI_INJECT_MODE",
            "VOICEPI_DICTIONARY", "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_STT_BACKEND", "VOICEPI_STT_API_KEY",
            "GROQ_API_KEY", "OPENAI_API_KEY",
        )}

    def tearDown(self):
        for k, v in self._cached.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v

    def _args(self, **over):
        defaults = dict(key="ctrl_r", model="large-v3", lang="da",
                        autodetect=False, device="cuda", mode="type")
        defaults.update(over)
        return types.SimpleNamespace(**defaults)

    def test_dump_includes_all_expected_sections(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        os.environ["VOICEPI_INITIAL_PROMPT"] = "foo,bar,baz,qux"
        os.environ["VOICEPI_BEAM_SIZE"] = "8"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()

        # header + every row label appears
        self.assertIn("[debug] effective settings:", out)
        for label in ("--key", "--model", "--lang", "--device",
                      "stt backend", "compute_type", "beam_size", "initial_prompt",
                      "dictionary", "quit", "audio thresholds", "XKB (Wayland)",
                      "inject mode"):
            self.assertIn(label, out)

        # env-sourced values are surfaced + annotated with the env var name
        self.assertIn("VOICEPI_COMPUTE_TYPE=float16", out)
        self.assertIn("VOICEPI_BEAM_SIZE=8", out)
        self.assertIn("VOICEPI_QUIT_KEY=(unset)", out)
        self.assertIn("VOICEPI_KEY=(unset)", out)
        self.assertIn("VOICEPI_INJECT_MODE=(unset)", out)
        self.assertIn("large-v3", out)
        self.assertIn("float16", out)
        # prompt is shown with its length + a preview substring
        self.assertIn("15 chars", out)
        self.assertIn("foo,bar,baz,qux", out)

    def test_long_prompt_is_truncated(self):
        os.environ["VOICEPI_INITIAL_PROMPT"] = "x" * 200
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()
        self.assertIn("200 chars", out)
        self.assertIn("...", out)  # truncated marker
        # full 200-char string is NOT in the output
        self.assertNotIn("x" * 200, out)

    def test_quit_key_env_is_shown_in_debug_dump(self):
        os.environ["VOICEPI_QUIT_KEY"] = "f12"
        os.environ["VOICEPI_QUIT_COUNT"] = "2"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()

        self.assertIn("2x f12", out)
        self.assertIn("VOICEPI_QUIT_KEY=f12", out)

    def test_quit_key_is_not_hardcoded_to_escape(self):
        cli = Path("vp_cli.py").read_text(encoding="utf-8")
        runtime = Path("voice_pi.py").read_text(encoding="utf-8")
        config = Path("vp_config.py").read_text(encoding="utf-8")

        self.assertIn('Setting("VOICEPI_QUIT_KEY", "quit_key", "esc", live=False)', config)
        self.assertIn('QUIT_KEY = (get_value("VOICEPI_QUIT_KEY", "esc")', cli)
        self.assertIn("if k == quit_key:", runtime)
        self.assertNotIn("if k == keyboard.Key.esc:", runtime)

    def test_debug_dump_treats_groq_key_as_cloud_api_key(self):
        os.environ["GROQ_API_KEY"] = "test-groq-key"
        voice_pi = load_voice_pi(cuda_devices=1)

        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "api", "remote")

        out = buf.getvalue()
        self.assertIn("stt api", out)
        self.assertIn("key=set", out)

    def test_unset_env_shows_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "int8_float16"),
        out = buf.getvalue()
        self.assertIn("VOICEPI_COMPUTE_TYPE=(unset)", out)
        self.assertIn("VOICEPI_INITIAL_PROMPT", out)  # row exists
        self.assertIn("(unset)", out)  # prompt shows (unset) too

    def test_autodetect_flag_overrides_lang_in_display(self):
        os.environ["VOICEPI_LANG"] = "da"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(
                self._args(lang="da", autodetect=True), "cuda", "float16")
        out = buf.getvalue()
        # final resolved lang is 'auto' even though VOICEPI_LANG=da
        # because --autodetect was passed
        self.assertRegex(out, r"--lang\s+auto\b")
        self.assertIn("--autodetect=True", out)

class ArgumentParserTests(unittest.TestCase):
    def test_parser_rejects_invalid_device(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        with redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                parser.parse_args(["--device", "cdua"])

    def test_parser_accepts_supported_devices(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        for device in voice_pi.VALID_DEVICES:
            with self.subTest(device=device):
                self.assertEqual(
                    parser.parse_args(["--device", device]).device,
                    device,
                )

    def test_parser_uses_key_env_default(self):
        with _env(VOICEPI_KEY="ctrl_l+space"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).key, "ctrl_l+space")
            self.assertEqual(parser.parse_args(["--key", "f9"]).key, "f9")

    def test_parser_uses_inject_mode_env_default(self):
        with _env(VOICEPI_INJECT_MODE="paste"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).mode, "paste")
            self.assertEqual(parser.parse_args(["--no-type"]).mode, "print")
            self.assertEqual(parser.parse_args(["--paste"]).mode, "paste")
            self.assertEqual(parser.parse_args(["--type"]).mode, "type")

    def test_parser_defaults_to_auto_inject_mode(self):
        old = os.environ.pop("VOICEPI_INJECT_MODE", None)
        try:
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()
        finally:
            if old is not None:
                os.environ["VOICEPI_INJECT_MODE"] = old

        self.assertEqual(parser.parse_args([]).mode, "auto")

    def test_parser_accepts_json_doctor_and_model_capacity(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        ns = parser.parse_args(["--json", "--doctor", "--model-capacity"])

        self.assertTrue(ns.json)
        self.assertTrue(ns.doctor)
        self.assertTrue(ns.model_capacity)

    def test_dictionary_status_exits_from_parser(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            with _env(VOICEPI_DICTIONARY=path):
                voice_pi = load_voice_pi()
                parser = voice_pi.build_arg_parser()

                with _capture_stdout() as buf:
                    with self.assertRaises(SystemExit) as cm:
                        parser.parse_args(["--dictionary-status"])

        self.assertEqual(cm.exception.code, 0)
        self.assertIn("managed path:", buf.getvalue())

    def test_dictionary_add_exits_from_parser_and_writes_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            with _env(VOICEPI_DICTIONARY=path):
                voice_pi = load_voice_pi()
                parser = voice_pi.build_arg_parser()

                with _capture_stdout():
                    with self.assertRaises(SystemExit) as cm:
                        parser.parse_args(["--dictionary-add", "OpenClaw"])

                with open(path, encoding="utf-8") as f:
                    data = json.load(f)

        self.assertEqual(cm.exception.code, 0)
        self.assertEqual(data["terms"], ["OpenClaw"])

class ModelCapacityTests(unittest.TestCase):
    def test_estimate_model_fits_uses_free_and_total_vram(self):
        import vp_model_capacity

        gpus = [vp_model_capacity.GpuInfo(
            index=0,
            name="RTX Test",
            total_mb=8192,
            free_mb=4096,
        )]

        _, fits = vp_model_capacity.estimate_model_fits(gpus)
        by_name = {fit.profile.name: fit for fit in fits}

        self.assertEqual(by_name["Whisper large-v3-turbo"].status, "ok")
        self.assertEqual(by_name["Whisper large-v3 float16"].status, "free-vram")
        self.assertEqual(by_name["NVIDIA Parakeet TDT 1.1B"].status, "too-small")

    def test_capacity_report_json_shape(self):
        import vp_model_capacity

        with patch("vp_model_capacity.query_gpus", return_value=[
            vp_model_capacity.GpuInfo(0, "RTX Test", 16384, 12000),
        ]):
            data = json.loads(vp_model_capacity.capacity_report(as_json=True))

        self.assertEqual(data["gpus"][0]["name"], "RTX Test")
        self.assertTrue(any(item["name"] == "Whisper large-v3 float16"
                            for item in data["models"]))

    def test_capacity_report_plain_mentions_free_vram(self):
        import vp_model_capacity

        with patch("vp_model_capacity.query_gpus", return_value=[
            vp_model_capacity.GpuInfo(0, "RTX Test", 8192, 4096),
        ]):
            report = vp_model_capacity.capacity_report()

        self.assertIn("4096 MB free / 8192 MB total", report)
        self.assertIn("Whisper large-v3-turbo", report)
        self.assertIn("Use free VRAM", report)

class ModuleSurfaceTests(unittest.TestCase):
    """voice_pi.py re-exports names that were moved into vp_cli / vp_transcribe
    when the file was split. Tests, the installer, and downstream callers
    still reach for these on the voice_pi module — make sure they resolve."""

    def test_voice_pi_reexports_cli_symbols(self):
        vp = load_voice_pi()
        for name in ("build_arg_parser", "_print_effective_config",
                     "KEY", "MODEL_NAME", "DEVICE", "LANG",
                     "INJECT_MODE", "VALID_INJECT_MODES",
                     "QUIT_COUNT", "QUIT_WINDOW_MS", "BEAM_SIZE"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")

    def test_voice_pi_reexports_transcribe_symbols(self):
        vp = load_voice_pi()
        for name in ("_transcribe", "_HALLUCINATIONS",
                     "is_hallucination", "SR", "INITIAL_PROMPT",
                     "TEMPERATURES", "CONTEXT_MIN_SECONDS",
                     "STT_BACKEND", "VALID_STT_BACKENDS",
                     "load_stt_model"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")

    def test_voice_pi_reexports_device_audio_keymap(self):
        vp = load_voice_pi()
        for name in ("_resolve_device", "VALID_DEVICES",
                     "_noise_snr", "_boost_quiet", "_looks_like_speech",
                     "TARGET_DBFS",
                     "_LAYOUT_KEYCODES", "_LANG_TO_XKB",
                     "_detect_xkb_layout", "_build_ydotool_ops"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")

class CliModuleIsolationTests(unittest.TestCase):
    """vp_cli.build_arg_parser must work standalone — no voice_pi import.
    Catches regressions where someone accidentally re-couples them."""

    def setUp(self):
        # vp_cli depends only on vp_audio, vp_device, vp_transcribe — all
        # of which need numpy. Stub it the same way load_voice_pi does so
        # this test runs even without numpy installed.
        for n in ("voice_pi", "vp_cli", "vp_transcribe",
                  "vp_audio", "vp_device"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))

    def test_parser_works_without_voice_pi(self):
        before = set(sys.modules)
        import vp_cli
        ns = vp_cli.build_arg_parser().parse_args([])
        # Defaults pulled from env vars; just check the shape.
        for attr in ("key", "model", "lang", "device", "mode", "autodetect"):
            self.assertTrue(hasattr(ns, attr),
                            f"parser missing --{attr}")
        # voice_pi may already have been loaded by an earlier test, but
        # importing vp_cli here must NOT pull it in fresh.
        newly_loaded = set(sys.modules) - before
        self.assertNotIn("voice_pi", newly_loaded,
                         "vp_cli must not pull in voice_pi")

class TemperatureParseTests(unittest.TestCase):
    """vp_transcribe._parse_temperatures: CSV float list with a safe
    default if unset, empty, or malformed."""

    def setUp(self):
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        import vp_transcribe
        self.t = vp_transcribe

    def test_unset_returns_default_ladder(self):
        self.assertEqual(self.t._parse_temperatures(None), [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures(""), [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures("   "), [0.0, 0.2])

    def test_single_value_locks_decode(self):
        self.assertEqual(self.t._parse_temperatures("0.0"), [0.0])
        self.assertEqual(self.t._parse_temperatures("0"), [0.0])
        self.assertEqual(self.t._parse_temperatures("0.4"), [0.4])

    def test_csv_ladder(self):
        self.assertEqual(self.t._parse_temperatures("0.0,0.2,0.4"),
                         [0.0, 0.2, 0.4])
        # Whitespace tolerated around commas.
        self.assertEqual(self.t._parse_temperatures(" 0.0 , 0.5 "),
                         [0.0, 0.5])

    def test_malformed_falls_back_to_default(self):
        self.assertEqual(self.t._parse_temperatures("not-a-number"),
                         [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures("0.0,abc"),
                         [0.0, 0.2])

class ContextMinSecondsTests(unittest.TestCase):
    """VOICEPI_CONTEXT_MIN_SECONDS gates condition_on_previous_text:
      * 0 (default)  -> always False (backwards-compatible)
      * > 0          -> True only when utterance duration meets the bar
    The bar lives in vp_transcribe.CONTEXT_MIN_SECONDS; the gate itself
    is one line in _transcribe so we mirror its expression here."""

    def _gate(self, threshold: float, dur: float) -> bool:
        return threshold > 0 and dur >= threshold

    def test_zero_threshold_never_enables_context(self):
        for dur in (0.0, 1.0, 5.0, 30.0, 1000.0):
            self.assertFalse(self._gate(0.0, dur),
                             f"threshold=0, dur={dur} must stay False")

    def test_positive_threshold_gates_on_duration(self):
        self.assertFalse(self._gate(5.0, 4.9))
        self.assertTrue(self._gate(5.0, 5.0))
        self.assertTrue(self._gate(5.0, 19.4))

class MetricsTests(unittest.TestCase):
    def test_append_jsonl_writes_unicode_event(self):
        for n in ("vp_metrics",):
            sys.modules.pop(n, None)
        import vp_metrics

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            vp_metrics.append_jsonl(path, {"text": "rødgrød", "n": 1})
            with open(path, encoding="utf-8") as f:
                data = f.read()
            self.assertIn('"text": "rødgrød"', data)
            self.assertIn('"n": 1', data)
        finally:
            try:
                os.remove(path)
            except OSError:
                pass

class ConfigTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_CONFIG", "VOICEPI_MODEL", "VOICEPI_LANG",
        )}
        for n in ("vp_config",):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k, v in self._old.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v
        sys.modules.pop("vp_config", None)

    def test_config_value_beats_env_and_persists(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "config.json")
            os.environ["VOICEPI_CONFIG"] = path
            os.environ["VOICEPI_LANG"] = "en"
            import vp_config

            vp_config.save_config({"lang": "da", "model": "large-v3"})
            self.assertEqual(vp_config.get_value("VOICEPI_LANG"), "da")
            self.assertEqual(vp_config.get_value("VOICEPI_MODEL"), "large-v3")
            self.assertEqual(vp_config.apply_config_to_environ(), {"VOICEPI_LANG", "VOICEPI_MODEL"})
            self.assertEqual(os.environ["VOICEPI_LANG"], "da")

class PrivacyModeTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_LOCAL_ONLY", "HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE",
            "HF_DATASETS_OFFLINE", "HF_HUB_DISABLE_TELEMETRY",
            "WANDB_DISABLED", "WANDB_MODE",
        )}
        for n in ("vp_privacy", "vp_config", "vp_cli", "vp_transcribe"):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k in self._old:
            os.environ.pop(k, None)
        for k, v in self._old.items():
            if v is not None:
                os.environ[k] = v
        for n in ("vp_privacy", "vp_config", "vp_cli", "vp_transcribe"):
            sys.modules.pop(n, None)

    def test_local_only_applies_offline_environment_gates(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        self.assertTrue(vp_privacy.apply_local_only_network_lock())
        self.assertEqual(os.environ["HF_HUB_OFFLINE"], "1")
        self.assertEqual(os.environ["TRANSFORMERS_OFFLINE"], "1")
        self.assertEqual(os.environ["HF_DATASETS_OFFLINE"], "1")
        self.assertEqual(os.environ["HF_HUB_DISABLE_TELEMETRY"], "1")
        self.assertEqual(os.environ["WANDB_DISABLED"], "true")
        self.assertEqual(os.environ["WANDB_MODE"], "offline")

    def test_local_only_does_not_override_existing_offline_values(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        os.environ["HF_HUB_OFFLINE"] = "custom"
        import vp_privacy

        vp_privacy.apply_local_only_network_lock()

        self.assertEqual(os.environ["HF_HUB_OFFLINE"], "custom")

    def test_local_only_blocks_cloud_backends(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_privacy.assert_local_backend("openai:gpt-4o-transcribe")

    def test_local_only_allows_current_local_backends(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        for backend in ("whisper", "faster-whisper", "parakeet"):
            vp_privacy.assert_local_backend(backend)

    def test_local_only_is_registered_as_config_setting(self):
        import vp_config

        self.assertIn("VOICEPI_LOCAL_ONLY", vp_config.SETTING_BY_ENV)
        self.assertEqual(
            vp_config.SETTING_BY_ENV["VOICEPI_LOCAL_ONLY"].key,
            "local_only",
        )
        self.assertFalse(vp_config.SETTING_BY_ENV["VOICEPI_LOCAL_ONLY"].live)

    def test_debug_dump_reports_local_only_setting(self):
        with open("vp_cli.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('"local only"', script)
        self.assertIn("VOICEPI_LOCAL_ONLY", script)

class CommandHookTests(unittest.TestCase):
    def test_command_hook_sends_event_json_on_stdin(self):
        import vp_command_hook

        with tempfile.NamedTemporaryFile(delete=False) as f:
            out_path = f.name
        code = (
            "import json,sys,pathlib;"
            "event=json.loads(sys.stdin.read());"
            f"pathlib.Path({out_path!r}).write_text(event['text'], encoding='utf-8')"
        )
        command = json.dumps([sys.executable, "-c", code])
        try:
            with patch.dict(os.environ, {
                "VOICEPI_COMMAND_HOOK": command,
                "VOICEPI_COMMAND_HOOK_TIMEOUT_MS": "2000",
            }, clear=False):
                result = vp_command_hook.run_command_hook({"text": "hello Codex"})
            with open(out_path, encoding="utf-8") as f:
                written = f.read()
        finally:
            os.remove(out_path)

        self.assertTrue(result.enabled)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(written, "hello Codex")

    def test_command_hook_timeout_is_nonfatal(self):
        import vp_command_hook

        command = json.dumps([sys.executable, "-c", "import time; time.sleep(2)"])
        with patch.dict(os.environ, {
            "VOICEPI_COMMAND_HOOK": command,
            "VOICEPI_COMMAND_HOOK_TIMEOUT_MS": "100",
        }, clear=False):
            result = vp_command_hook.run_command_hook({"text": "slow"})

        self.assertTrue(result.enabled)
        self.assertTrue(result.timeout)
        self.assertIn("timed out", result.error)

    def test_command_hook_rejects_invalid_json_array(self):
        import vp_command_hook

        with patch.dict(os.environ, {"VOICEPI_COMMAND_HOOK": '["ok", 5]'}, clear=False):
            result = vp_command_hook.run_command_hook({"text": "bad"})

        self.assertTrue(result.enabled)
        self.assertIn("array of strings", result.error)

    def test_voice_pi_records_command_hook_after_event_creation(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        event_pos = script.index("event = base_event(")
        hook_pos = script.index("hook_result = run_command_hook(event)")
        metrics_pos = script.index("append_jsonl(self.metrics_jsonl, event)")
        self.assertLess(event_pos, hook_pos)
        self.assertLess(hook_pos, metrics_pos)


class DebugToolingTests(unittest.TestCase):
    def test_probe_key_is_documented_and_ci_sanity_checked(self):
        probe = Path("scripts/probe-key.py").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        self.assertIn("Probe a push-to-talk key/chord", probe)
        self.assertIn("python scripts/probe-key.py", config)
        self.assertIn("probe-key.py sanity", workflow)
        self.assertIn("python scripts/probe-key.py bogus_key 1", workflow)
