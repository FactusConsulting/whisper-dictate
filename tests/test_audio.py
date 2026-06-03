from tests.test_helpers import (
    _capture_stdout,
    _env,
    io,
    json,
    load_voice_pi_realnp,
    os,
    patch,
    Path,
    redirect_stderr,
    sys,
    unittest,
)

class AudioDspTests(unittest.TestCase):
    """Characterisation tests for the audio DSP with REAL numpy. These pin
    current behaviour so the upcoming vp_audio.py extraction is provably
    behaviour-preserving (same asserts, only the import path changes)."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.vp = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import numpy as np
        cls.np = np

    # --- _noise_snr ---
    def test_noise_snr_too_few_frames(self):
        a = self.np.zeros(1000, dtype=self.np.float32)
        self.assertEqual(self.vp._noise_snr(a), (-90.0, 0.0))

    def test_noise_snr_constant_signal(self):
        a = self.np.full(480 * 8, 0.5, dtype=self.np.float32)
        noise, snr = self.vp._noise_snr(a)
        self.assertAlmostEqual(noise, -6.0206, places=2)
        self.assertAlmostEqual(snr, 0.0, places=6)

    def test_noise_snr_contrast_has_high_snr(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 1.0 if i % 2 == 0 else 0.001, dtype=np.float32)
            for i in range(10)])
        noise, snr = self.vp._noise_snr(a)
        self.assertGreater(snr, 40.0)
        self.assertLess(noise, -40.0)

    # --- _boost_quiet ---
    def test_boost_quiet_normalises_toward_target(self):
        np = self.np
        a = np.full(1920, 0.01, dtype=np.float32)
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertEqual(out.dtype, np.float32)
        rms = float(np.sqrt(np.mean(out ** 2)))
        self.assertAlmostEqual(20 * np.log10(rms), self.vp.TARGET_DBFS,
                               places=1)

    def test_boost_quiet_never_clips(self):
        np = self.np
        a = np.zeros(1920, dtype=np.float32)
        a[:10] = 0.9
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertLessEqual(float(np.max(np.abs(out))), 0.99 + 1e-6)

    def test_cap_line_is_bold_on_interactive_terminal(self):
        import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR=None, VOICEPI_NO_COLOR=None):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "\033[1m[cap] raw=-20dBFS\033[0m",
                )

    def test_cap_line_stays_plain_for_piped_output(self):
        import vp_audio

        class Pipe:
            def isatty(self):
                return False

        with patch.object(vp_audio.sys, "stdout", Pipe()):
            self.assertEqual(
                vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                "[cap] raw=-20dBFS",
            )

    def test_cap_line_highlight_respects_no_color(self):
        import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )
            with _env(VOICEPI_NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )

    # --- _looks_like_speech ---
    def test_looks_like_speech_rejects_too_quiet(self):
        a = self.np.full(1920, 1e-4, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("too quiet", msg)

    def test_looks_like_speech_rejects_flat_signal(self):
        a = self.np.full(1920, 0.1, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("no speech contrast", msg)

    def test_looks_like_speech_accepts_contrasted_speech(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(10)])
        ok, _ = self.vp._looks_like_speech(a)
        self.assertTrue(ok)

class AudioDuckingTests(unittest.TestCase):
    def test_audio_ducker_restore_resets_saved_session_volumes(self):
        import vp_audio_ducking

        class Volume:
            def __init__(self):
                self.values = []

            def SetMasterVolume(self, value, _context):
                self.values.append(value)

        first = Volume()
        second = Volume()
        ducker = vp_audio_ducking.AudioDucker(enabled=True, target_volume=0.25)
        ducker._sessions = [(first, 0.8), (second, 0.6)]

        ducker.exit()

        self.assertEqual(first.values, [0.8])
        self.assertEqual(second.values, [0.6])
        self.assertEqual(ducker._sessions, [])

    def test_audio_ducker_config_is_disabled_by_default_and_clamps_level(self):
        with _env(VOICEPI_AUDIO_DUCKING=None, VOICEPI_AUDIO_DUCKING_LEVEL="2.5"):
            sys.modules.pop("vp_audio_ducking", None)
            import vp_audio_ducking

            ducker = vp_audio_ducking.AudioDucker.from_config()

        self.assertFalse(ducker.enabled)
        self.assertEqual(ducker.target_volume, 1.0)

class CalibrationTests(unittest.TestCase):
    def test_calibration_analysis_passes_clean_audio(self):
        np = AudioDspTests.np
        sys.modules["numpy"] = np
        sys.modules.pop("vp_calibration", None)
        import vp_calibration

        quiet = np.full(4000, 0.001, dtype=np.float32)
        speech = np.full(12000, 0.15, dtype=np.float32)
        pcm = (np.concatenate([quiet, speech]) * 32767).astype(np.int16)

        result = vp_calibration.analyze_calibration_audio(pcm)

        self.assertEqual(result["event"], "mic_calibration")
        self.assertEqual(result["status"], "pass")
        self.assertGreater(result["snr_db"], 15)
        self.assertIn("VOICEPI_MIN_INPUT_DBFS", result["recommended"])

    def test_calibration_analysis_warns_on_low_snr(self):
        np = AudioDspTests.np
        sys.modules["numpy"] = np
        sys.modules.pop("vp_calibration", None)
        import vp_calibration

        pcm = (np.full(16000, 0.01, dtype=np.float32) * 32767).astype(np.int16)

        result = vp_calibration.analyze_calibration_audio(pcm)

        self.assertIn(result["status"], ("warn", "fail"))
        self.assertTrue(result["warnings"])

    def test_calibration_json_output_is_single_json_object(self):
        import vp_calibration

        result = {
            "event": "mic_calibration",
            "status": "pass",
            "warnings": [],
            "raw_dbfs": -20.0,
            "noise_dbfs": -60.0,
            "snr_db": 40.0,
            "peak": 0.5,
            "recommended": {},
        }
        with _capture_stdout() as buf:
            vp_calibration.print_calibration_result(result, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), result)

    def test_parser_accepts_calibration_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic"])
        self.assertEqual(args.calibrate_mic, 5.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic", "3"])
        self.assertEqual(args.calibrate_mic, 3.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-file", "sample.wav"])
        self.assertEqual(args.calibrate_file, "sample.wav")

    def test_voice_pi_handles_calibration_before_model_load(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        calibration = script.index("if a.calibrate_mic is not None or a.calibrate_file")
        model_load = script.index("_model = load_stt_model")
        self.assertLess(calibration, model_load)

    def test_cloud_backend_uses_api_device_and_no_local_model_ready_log(self):
        script = Path("voice_pi.py").read_text(encoding="utf-8")

        backend_lookup = script.index("backend = STT_BACKEND")
        cloud_device = script.index('if backend == "openai":\n        dev, ctype = "api", "remote"')
        device_resolve = script.index("dev, ctype = _resolve_device(a.device)")
        model_load = script.index("_model = load_stt_model(loaded_model_name, dev, ctype)")
        api_ready = script.index('print(f"api ready in {_model_load_s:.1f}s"')

        self.assertLess(backend_lookup, cloud_device)
        self.assertLess(cloud_device, device_resolve)
        self.assertLess(model_load, api_ready)
        self.assertIn("using {label} {loaded_model_name} via configured API", script)

    def test_cloud_model_load_runtime_error_is_reported_without_traceback(self):
        script = Path("voice_pi.py").read_text(encoding="utf-8")

        model_load = script.index("_model = load_stt_model(loaded_model_name, dev, ctype)")
        before = script.rfind("try:", 0, model_load)
        after = script.index("except RuntimeError as e:", model_load)

        self.assertLess(before, model_load)
        self.assertIn('emit_worker_event("error"', script[model_load:after + 300])
        self.assertIn("startup error", script[model_load:after + 300])
        self.assertIn("raise SystemExit(1)", script[model_load:after + 300])

class WorkerEventTests(unittest.TestCase):
    def test_worker_events_disabled_by_default(self):
        import vp_worker_events

        with patch.dict(os.environ, {}, clear=True):
            buf = io.StringIO()
            with redirect_stderr(buf):
                vp_worker_events.emit("status", state="ready")
            self.assertEqual(buf.getvalue(), "")

    def test_worker_events_emit_compact_json_on_stderr(self):
        import vp_worker_events

        with patch.dict(os.environ, {"VOICEPI_WORKER_EVENTS": "1"}, clear=True):
            buf = io.StringIO()
            with redirect_stderr(buf):
                vp_worker_events.emit("status", state="ready", model="large-v3")
            line = buf.getvalue().strip()

        self.assertTrue(line.startswith("[worker-event] "))
        payload = json.loads(line.removeprefix("[worker-event] "))
        self.assertEqual(payload, {
            "event": "status",
            "state": "ready",
            "model": "large-v3",
        })
