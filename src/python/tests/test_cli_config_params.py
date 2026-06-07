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
        # Live-config application moved into vp_dictate._apply_effective_config.
        dictate = Path("src/python/whisper_dictate/vp_dictate.py").read_text(encoding="utf-8")
        ctx = next(s for s in schema["settings"] if s["key"] == "context_min_seconds")
        self.assertEqual(
            (ctx["env"], ctx["default"], ctx["live"]),
            ("VOICEPI_CONTEXT_MIN_SECONDS", "5", True),
        )
        self.assertIn('get_value("VOICEPI_CONTEXT_MIN_SECONDS", "5") or "5"', transcribe)
        self.assertIn('after.get("context_min_seconds", "5")', dictate)
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
        # Live-config application moved into vp_dictate._apply_effective_config.
        dictate = Path("src/python/whisper_dictate/vp_dictate.py").read_text(encoding="utf-8")

        vad = next(s for s in schema["settings"] if s["key"] == "vad_speech_pad_ms")
        self.assertEqual((vad["env"], vad["default"]), ("VOICEPI_VAD_SPEECH_PAD_MS", "200"))
        self.assertIn('VAD_SPEECH_PAD_MS = int(get_value("VOICEPI_VAD_SPEECH_PAD_MS", "200")', transcribe)
        self.assertIn("speech_pad_ms=VAD_SPEECH_PAD_MS", transcribe)
        self.assertIn('vp_transcribe.VAD_SPEECH_PAD_MS = int(after.get("vad_speech_pad_ms", "200"))', dictate)

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
