import subprocess

from helpers import (
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
        cli = Path("src/python/whisper_dictate/vp_cli.py").read_text(encoding="utf-8")
        runtime = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")
        schema = json.loads(
            Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        )

        quit_key = next(s for s in schema["settings"] if s["key"] == "quit_key")
        self.assertEqual(
            (quit_key["env"], quit_key["default"], quit_key["live"]),
            ("VOICEPI_QUIT_KEY", "esc", False),
        )
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

class ModuleSurfaceTests(unittest.TestCase):
    """runtime.py exposes names that were moved into focused package modules."""

    def test_runtime_reexports_cli_symbols(self):
        vp = load_voice_pi()
        for name in ("build_arg_parser", "_print_effective_config",
                     "KEY", "MODEL_NAME", "DEVICE", "LANG",
                     "INJECT_MODE", "VALID_INJECT_MODES",
                     "QUIT_COUNT", "QUIT_WINDOW_MS", "BEAM_SIZE"):
            self.assertTrue(hasattr(vp, name),
                            f"runtime.{name} missing - re-export broken")

    def test_runtime_reexports_transcribe_symbols(self):
        vp = load_voice_pi()
        for name in ("_transcribe", "_HALLUCINATIONS",
                     "is_hallucination", "SR", "INITIAL_PROMPT",
                     "TEMPERATURES", "CONTEXT_MIN_SECONDS",
                     "STT_BACKEND", "VALID_STT_BACKENDS",
                     "load_stt_model"):
            self.assertTrue(hasattr(vp, name),
                            f"runtime.{name} missing - re-export broken")

    def test_runtime_reexports_device_audio_keymap(self):
        vp = load_voice_pi()
        for name in ("_resolve_device", "VALID_DEVICES",
                     "_noise_snr", "_boost_quiet", "_looks_like_speech",
                     "TARGET_DBFS",
                     "_LANG_TO_XKB", "_detect_xkb_layout"):
            self.assertTrue(hasattr(vp, name),
                            f"runtime.{name} missing - re-export broken")


class PythonPackageLayoutTests(unittest.TestCase):
    def test_runtime_is_real_package_module_without_root_shim(self):
        runtime = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")

        self.assertFalse(Path("voice_pi.py").exists())
        self.assertIn("def main() -> None:", runtime)
        self.assertIn('if __name__ == "__main__":\n    main()', runtime)

    def test_runtime_python_files_are_discoverable_in_package(self):
        root_modules = sorted(Path(".").glob("vp_*.py"))
        package_modules = sorted(Path("src/python/whisper_dictate").glob("*.py"))
        expected_modules = {
            "__init__.py",
            "runtime.py",
            "vp_audio.py",
            "vp_audio_ducking.py",
            "vp_benchmark.py",
            "vp_cli.py",
            "vp_config.py",
            "vp_dictionary_suggest.py",
            "vp_doctor.py",
            "vp_external_api.py",
            "vp_inject.py",
            "vp_parakeet.py",
            "vp_postprocess.py",
            "vp_rust.py",
            "vp_transcribe.py",
        }

        self.assertEqual([], root_modules)
        self.assertEqual(expected_modules, {path.name for path in package_modules})
        self.assertTrue(all(path.is_file() for path in package_modules))
        self.assertEqual(len(package_modules), len(set(package_modules)))


class CliModuleIsolationTests(unittest.TestCase):
    """vp_cli.build_arg_parser must work standalone - no runtime import.
    Catches regressions where someone accidentally re-couples them."""

    def setUp(self):
        # vp_cli depends only on vp_audio and vp_transcribe for this dump — both
        # of which need numpy. Stub it the same way load_voice_pi does so
        # this test runs even without numpy installed.
        for n in ("voice_pi", "vp_cli", "vp_transcribe",
                  "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))

    def test_parser_works_without_voice_pi(self):
        before = set(sys.modules)
        from whisper_dictate import vp_cli
        ns = vp_cli.build_arg_parser().parse_args([])
        # Defaults pulled from env vars; just check the shape.
        for attr in ("key", "model", "lang", "device", "mode", "autodetect"):
            self.assertTrue(hasattr(ns, attr),
                            f"parser missing --{attr}")
        # The runtime may already have been loaded by an earlier test, but
        # importing vp_cli here must not pull it in fresh.
        newly_loaded = set(sys.modules) - before
        self.assertNotIn("whisper_dictate.runtime", newly_loaded,
                         "vp_cli must not pull in runtime")

class TemperatureParseTests(unittest.TestCase):
    """vp_transcribe._parse_temperatures: CSV float list with a safe
    default if unset, empty, or malformed."""

    def setUp(self):
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        from whisper_dictate import vp_transcribe
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
      * 5 (default)  -> True for utterances at least five seconds long
      * 0            -> always False
      * > 0          -> True only when utterance duration meets the bar
    The bar lives in vp_transcribe.CONTEXT_MIN_SECONDS; the gate itself
    is one line in _transcribe so we mirror its expression here."""

    def _gate(self, threshold: float, dur: float) -> bool:
        return threshold > 0 and dur >= threshold

    def test_zero_threshold_never_enables_context(self):
        for dur in (0.0, 1.0, 5.0, 30.0, 1000.0):
            self.assertFalse(self._gate(0.0, dur),
                             f"threshold=0, dur={dur} must stay False")

    def test_default_threshold_is_five_seconds(self):
        schema = json.loads(
            Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        )
        transcribe = Path("src/python/whisper_dictate/vp_transcribe.py").read_text(encoding="utf-8")
        runtime = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")
        ctx = next(s for s in schema["settings"] if s["key"] == "context_min_seconds")
        self.assertEqual(
            (ctx["env"], ctx["default"], ctx["live"]),
            ("VOICEPI_CONTEXT_MIN_SECONDS", "5", True),
        )
        self.assertIn('get_value("VOICEPI_CONTEXT_MIN_SECONDS", "5") or "5"', transcribe)
        self.assertIn('after.get("context_min_seconds", "5")', runtime)
        self.assertFalse(self._gate(5.0, 4.9))
        self.assertTrue(self._gate(5.0, 5.0))

    def test_positive_threshold_gates_on_duration(self):
        self.assertFalse(self._gate(5.0, 4.9))
        self.assertTrue(self._gate(5.0, 5.0))
        self.assertTrue(self._gate(5.0, 19.4))


class VadSpeechPaddingTests(unittest.TestCase):
    def test_vad_speech_padding_is_configurable_and_passed_to_whisper(self):
        schema = json.loads(
            Path("src/python/whisper_dictate/settings_schema.json").read_text(encoding="utf-8")
        )
        transcribe = Path("src/python/whisper_dictate/vp_transcribe.py").read_text(encoding="utf-8")
        runtime = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")

        vad = next(s for s in schema["settings"] if s["key"] == "vad_speech_pad_ms")
        self.assertEqual((vad["env"], vad["default"]), ("VOICEPI_VAD_SPEECH_PAD_MS", "200"))
        self.assertIn('VAD_SPEECH_PAD_MS = int(get_value("VOICEPI_VAD_SPEECH_PAD_MS", "200")', transcribe)
        self.assertIn("speech_pad_ms=VAD_SPEECH_PAD_MS", transcribe)
        self.assertIn('vp_transcribe.VAD_SPEECH_PAD_MS = int(after.get("vad_speech_pad_ms", "200"))', runtime)

class ConfigTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_CONFIG", "VOICEPI_MODEL", "VOICEPI_LANG",
            "VOICEPI_XKB_LAYOUT",
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
            from whisper_dictate import vp_config

            vp_config.save_config({"lang": "da", "model": "large-v3", "xkb_layout": "dk"})
            self.assertEqual(vp_config.get_value("VOICEPI_LANG"), "da")
            self.assertEqual(vp_config.get_value("VOICEPI_MODEL"), "large-v3")
            self.assertEqual(vp_config.get_value("VOICEPI_XKB_LAYOUT"), "dk")
            self.assertEqual(
                vp_config.apply_config_to_environ(),
                {"VOICEPI_LANG", "VOICEPI_MODEL", "VOICEPI_XKB_LAYOUT"},
            )
            self.assertEqual(os.environ["VOICEPI_LANG"], "da")
            self.assertEqual(os.environ["VOICEPI_XKB_LAYOUT"], "dk")

class WindowsStdioEncodingTests(unittest.TestCase):
    def test_windows_stdio_keeps_interactive_console_native(self):
        voice_pi = load_voice_pi()

        class Stream:
            def __init__(self, tty):
                self.tty = tty
                self.calls = []

            def isatty(self):
                return self.tty

            def reconfigure(self, **kwargs):
                self.calls.append(kwargs)

        stdout = Stream(tty=True)
        stderr = Stream(tty=True)
        with patch.object(voice_pi.os, "name", "nt"), \
                patch.object(voice_pi.sys, "stdout", stdout), \
                patch.object(voice_pi.sys, "stderr", stderr):
            voice_pi._configure_windows_stdio()

        self.assertEqual(stdout.calls, [])
        self.assertEqual(stderr.calls, [])

    def test_windows_stdio_forces_utf8_for_piped_worker_output(self):
        voice_pi = load_voice_pi()

        class Stream:
            def __init__(self):
                self.calls = []

            def isatty(self):
                return False

            def reconfigure(self, **kwargs):
                self.calls.append(kwargs)

        stdout = Stream()
        stderr = Stream()
        with patch.object(voice_pi.os, "name", "nt"), \
                patch.object(voice_pi.sys, "stdout", stdout), \
                patch.object(voice_pi.sys, "stderr", stderr):
            voice_pi._configure_windows_stdio()

        expected = [{"encoding": "utf-8", "errors": "replace"}]
        self.assertEqual(stdout.calls, expected)
        self.assertEqual(stderr.calls, expected)


class YdotooldDoctorTests(unittest.TestCase):
    def test_process_detail_rejects_process_with_unready_socket(self):
        from whisper_dictate import runtime

        completed = subprocess.CompletedProcess(["pgrep", "-x", "ydotoold"], 0, stdout="9132\n")

        with patch("whisper_dictate.runtime.subprocess.run", return_value=completed):
            ok, detail = runtime._ydotoold_process_detail(socket_ready=False)

        self.assertFalse(ok)
        self.assertIn("socket is not accepting connections", detail)
        self.assertIn("9132", detail)

    def test_process_detail_accepts_ready_socket(self):
        from whisper_dictate import runtime

        ok, detail = runtime._ydotoold_process_detail(socket_ready=True)

        self.assertTrue(ok)
        self.assertEqual(detail, "accepting connections")


class DebugToolingTests(unittest.TestCase):
    def test_probe_key_is_documented_and_ci_sanity_checked(self):
        probe = Path("scripts/dev/probe-key.py").read_text(encoding="utf-8")
        config = Path("docs/CONFIGURATION.md").read_text(encoding="utf-8")
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        self.assertIn("Probe a push-to-talk key/chord", probe)
        self.assertIn("python scripts/dev/probe-key.py", config)
        self.assertIn("probe-key.py sanity", workflow)
        self.assertIn("python scripts/dev/probe-key.py bogus_key 1", workflow)
