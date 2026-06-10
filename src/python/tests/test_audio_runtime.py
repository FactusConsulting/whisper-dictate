from helpers import (
    _capture_stdout,
    _env,
    io,
    json,
    patch,
    Path,
    RealNumpyAudioCase,
    redirect_stderr,
    sys,
    types,
    unittest,
)

class RuntimeAudioDeviceTests(RealNumpyAudioCase):
    def setUp(self):
        from helpers import load_voice_pi

        self.runtime = load_voice_pi()

    def test_sounddevice_input_name_uses_default_input_index(self):
        class Default:
            device = (3, 7)

        class FakeSoundDevice:
            default = Default()

            def __init__(self):
                self.calls = []

            def query_devices(self, device=None, kind=None):
                self.calls.append((device, kind))
                return {"name": "USB Microphone", "max_input_channels": 2}

        fake = FakeSoundDevice()

        self.assertEqual(self.runtime._sounddevice_input_name(fake), "USB Microphone")
        self.assertEqual(fake.calls, [(3, None)])
        self.assertEqual(self.runtime._sounddevice_input_channels(fake), 2)
        self.assertEqual(self.runtime._sounddevice_capture_channel_candidates(2), [2, 1])

    def test_sounddevice_input_name_falls_back_to_default_input_query(self):
        class Default:
            device = (-1, 7)

        class FakeSoundDevice:
            default = Default()

            def __init__(self):
                self.calls = []

            def query_devices(self, device=None, kind=None):
                self.calls.append((device, kind))
                return {"name": "Default Array", "max_input_channels": 1}

        fake = FakeSoundDevice()

        self.assertEqual(self.runtime._sounddevice_input_name(fake), "Default Array")
        self.assertEqual(fake.calls, [(None, "input")])
        self.assertEqual(self.runtime._sounddevice_input_channels(fake), 1)
        self.assertEqual(self.runtime._sounddevice_capture_channel_candidates(12), [8, 2, 1])

    def test_sounddevice_stream_kwargs_try_low_latency_before_default(self):
        self.runtime.SR = 16000
        callback = object()

        low_latency, fallback = self.runtime._sounddevice_stream_kwargs(2, callback)

        self.assertEqual(low_latency["samplerate"], 16000)
        self.assertEqual(low_latency["channels"], 2)
        self.assertEqual(low_latency["dtype"], "int16")
        self.assertIs(low_latency["callback"], callback)
        self.assertEqual(low_latency["blocksize"], 320)
        self.assertEqual(low_latency["latency"], "low")
        self.assertEqual(fallback, {
            "samplerate": 16000,
            "channels": 2,
            "dtype": "int16",
            "callback": callback,
        })

    def test_sounddevice_start_falls_back_when_low_latency_is_rejected(self):
        self.runtime.SR = 16000
        calls = []

        class Stream:
            def __init__(self, **kwargs):
                calls.append(kwargs)
                if kwargs.get("latency") == "low":
                    raise RuntimeError("low latency unsupported")
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (3, 7)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "USB Microphone",
                "max_input_channels": 2,
            },
        )
        fake = types.SimpleNamespace(
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _stream=None,
            _cb=lambda *_args: None,
        )

        with patch.dict(sys.modules, {"sounddevice": fake_sd}):
            backend, device = self.runtime.Dictate._start_sounddevice(fake)

        self.assertEqual(backend, "sounddevice")
        self.assertEqual(device, "USB Microphone")
        self.assertEqual(fake._capture_channels, 2)
        self.assertTrue(fake._stream.started)
        self.assertEqual(calls[0]["latency"], "low")
        self.assertNotIn("latency", calls[1])
        self.assertEqual(calls[1]["channels"], 2)

    def test_runtime_worker_events_report_capture_state_and_audio_device(self):
        # The status/transcribe pipeline stays in vp_dictate; the low-level audio
        # capture (channel selection + metered "audio" events) moved to vp_capture.
        script = Path("src/python/whisper_dictate/vp_dictate.py").read_text(encoding="utf-8")
        capture = Path("src/python/whisper_dictate/vp_capture.py").read_text(encoding="utf-8")

        self.assertIn('state="recording"', script)
        self.assertIn('state="transcribing"', script)
        self.assertIn('state="ready"', script)
        self.assertIn("audio_device=self._audio_input_device", capture)
        self.assertIn("capture_backend=self._capture_backend", capture)
        self.assertIn("capture_channels=self._capture_channels", capture)
        self.assertIn("_sounddevice_capture_channel_candidates", capture)
        self.assertIn("channels=self._capture_channels", capture)
        self.assertIn("pcm = _select_active_channel_pcm(pcm).astype(np.int16)", script)
        self.assertIn('_emit_worker_event(\n            "audio"', capture)
        self.assertIn("level=round(level, 3)", capture)
        self.assertIn("raw_dbfs=round(raw_dbfs, 1)", capture)

    def test_worker_event_emits_structured_ascii_stderr_without_helper_process(self):
        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                self.runtime._emit_worker_event(
                    "audio",
                    state="recording",
                    audio_device="Mikrofon æøå",
                    level=0.25,
                )

        line = stderr.getvalue().strip()
        self.assertTrue(line.startswith("[worker-event] "))
        self.assertIn(r"Mikrofon \u00e6\u00f8\u00e5", line)
        payload = json.loads(line.removeprefix("[worker-event] "))
        self.assertEqual(payload["event"], "audio")
        self.assertEqual(payload["state"], "recording")
        self.assertEqual(payload["level"], 0.25)

    def test_record_utterance_event_emits_structured_audio_and_post_metadata(self):
        event = {
            "event": "utterance",
            "text": "Hej, mit navn er Sara.",
            "text_preview": "Hej, mit navn er Sara.",
            "audio_raw_dbfs": -33.2,
            "audio_peak": 0.282,
            "audio_noise_dbfs": -78.0,
            "audio_snr_db": 49.0,
            "audio_gain": 3.5,
            "post_processor": "groq",
            "post_mode": "clean",
            "post_model": "llama-3.3-70b-versatile",
            "post_changed": True,
            "dictionary_terms": ["Sara"],
            "dictionary_replacements": [{"from": "Lars datter", "to": "Lars' datter", "count": 1}],
        }
        fake = types.SimpleNamespace(metrics_jsonl=None, json_output=False)

        # _record_utterance_event lives in vp_dictate and resolves these helpers
        # from that module's namespace; patch them there to isolate the test from
        # the Rust command-hook / history side-effects.
        from whisper_dictate import vp_dictate
        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with patch.object(vp_dictate, "_run_command_hook_and_annotate", lambda _event: None):
                with patch.object(vp_dictate, "_append_jsonl", lambda *_args, **_kwargs: None):
                    with patch.object(vp_dictate, "_append_history", lambda _event: None):
                        with redirect_stderr(stderr):
                            self.runtime.Dictate._record_utterance_event(fake, event)

        payload = json.loads(stderr.getvalue().strip().removeprefix("[worker-event] "))
        self.assertEqual(payload["event"], "utterance")
        self.assertEqual(payload["text"], "Hej, mit navn er Sara.")
        self.assertEqual(payload["audio_raw_dbfs"], -33.2)
        self.assertEqual(payload["post_processor"], "groq")
        self.assertEqual(payload["dictionary_terms"], ["Sara"])

    def _run_record_utterance_event(self, *, json_output, metrics_jsonl):
        """Drive _record_utterance_event with side effects stubbed, returning the
        list of (path, event) tuples passed to _append_jsonl so callers can assert
        whether the metrics file would have been written."""
        from whisper_dictate import vp_dictate

        event = {"event": "utterance", "text": "hello"}
        fake = types.SimpleNamespace(
            metrics_jsonl=metrics_jsonl, json_output=json_output
        )
        jsonl_calls = []
        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with patch.object(vp_dictate, "_run_command_hook_and_annotate", lambda _e: None):
                with patch.object(
                    vp_dictate,
                    "_append_jsonl",
                    lambda path, ev: jsonl_calls.append((path, ev)),
                ):
                    with patch.object(vp_dictate, "_append_history", lambda _e: None):
                        with redirect_stderr(stderr):
                            self.runtime.Dictate._record_utterance_event(fake, event)
        return jsonl_calls

    def test_metrics_written_when_json_output_on_and_path_set(self):
        # JSON stdout enabled + a path -> the metrics file IS appended to.
        calls = self._run_record_utterance_event(
            json_output=True, metrics_jsonl="/tmp/metrics.jsonl"
        )
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "/tmp/metrics.jsonl")
        self.assertEqual(calls[0][1]["text"], "hello")

    def test_metrics_not_written_when_json_output_off(self):
        # JSON stdout disabled -> NO metrics write even with a path set.
        calls = self._run_record_utterance_event(
            json_output=False, metrics_jsonl="/tmp/metrics.jsonl"
        )
        self.assertEqual(calls, [])

    def test_first_audio_callback_sets_recording_start_event_and_time(self):
        np = self.np
        fake = types.SimpleNamespace(
            recording=True,
            frames=[],
            _first_audio_event=self.runtime.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _chunk: None,
        )
        chunk = np.ones((320, 1), dtype=np.int16)

        self.runtime.Dictate._cb(fake, chunk, len(chunk), None, None)

        self.assertTrue(fake._first_audio_event.is_set())
        self.assertGreater(fake._first_audio_at, 0.0)
        self.assertEqual(fake._record_started, fake._first_audio_at)
        self.assertEqual(len(fake.frames), 1)
        self.assertIsNot(fake.frames[0], chunk)

    def test_runtime_start_reports_opening_and_first_audio_without_persistent_capture(self):
        script = Path("src/python/whisper_dictate/vp_dictate.py").read_text(encoding="utf-8")
        start = script.split("def _start(self):", 1)[1].split("def _stop_and_transcribe", 1)[0]

        self.assertIn('self._first_audio_event.clear()', start)
        self.assertIn('self._record_keydown_at = time.monotonic()', start)
        self.assertIn('_emit_worker_event("status", state="opening")', start)
        self.assertIn('first_audio_ready = self._first_audio_event.wait(timeout=FIRST_AUDIO_WAIT_S)', start)
        self.assertIn('first_audio="ok" if first_audio_ready else "pending"', start)
        self.assertLess(start.index('state="opening"'), start.index("self._start_sounddevice()"))
        self.assertLess(start.index("self._start_sounddevice()"), start.index('state="recording"'))


class AudioDuckingTests(unittest.TestCase):
    def test_audio_ducker_restore_resets_saved_session_volumes(self):
        from whisper_dictate import runtime

        class Volume:
            def __init__(self):
                self.values = []

            def SetMasterVolume(self, value, _context):
                self.values.append(value)

        first = Volume()
        second = Volume()
        ducker = runtime.AudioDucker(enabled=True, target_volume=0.25)
        ducker._sessions = [(first, 0.8), (second, 0.6)]

        ducker.exit()

        self.assertEqual(first.values, [0.8])
        self.assertEqual(second.values, [0.6])
        self.assertEqual(ducker._sessions, [])

    def test_audio_ducker_config_is_disabled_by_default_and_clamps_level(self):
        with _env(VOICEPI_AUDIO_DUCKING=None, VOICEPI_AUDIO_DUCKING_LEVEL="2.5"):
            sys.modules.pop("whisper_dictate.runtime", None)
            from whisper_dictate import runtime

            ducker = runtime.AudioDucker.from_config()

        self.assertFalse(ducker.enabled)
        self.assertEqual(ducker.target_volume, 1.0)

class CalibrationTests(RealNumpyAudioCase):
    def test_calibration_analysis_passes_clean_audio(self):
        np = self.np
        sys.modules["numpy"] = np
        sys.modules.pop("whisper_dictate.runtime", None)
        from whisper_dictate import runtime

        quiet = np.full(4000, 0.001, dtype=np.float32)
        speech = np.full(12000, 0.15, dtype=np.float32)
        pcm = (np.concatenate([quiet, speech]) * 32767).astype(np.int16)

        result = runtime.analyze_calibration_audio(pcm)

        self.assertEqual(result["event"], "mic_calibration")
        self.assertEqual(result["status"], "pass")
        self.assertGreater(result["snr_db"], 15)
        self.assertIn("VOICEPI_MIN_INPUT_DBFS", result["recommended"])

    def test_calibration_analysis_warns_on_low_snr(self):
        np = self.np
        sys.modules["numpy"] = np
        sys.modules.pop("whisper_dictate.runtime", None)
        from whisper_dictate import runtime

        pcm = (np.full(16000, 0.01, dtype=np.float32) * 32767).astype(np.int16)

        result = runtime.analyze_calibration_audio(pcm)

        self.assertIn(result["status"], ("warn", "fail"))
        self.assertTrue(result["warnings"])

    def test_calibration_json_output_is_single_json_object(self):
        from whisper_dictate import runtime

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
            runtime.print_calibration_result(result, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), result)

    def test_parser_accepts_calibration_options(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic"])
        self.assertEqual(args.calibrate_mic, 5.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic", "3"])
        self.assertEqual(args.calibrate_mic, 3.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-file", "sample.wav"])
        self.assertEqual(args.calibrate_file, "sample.wav")

    def test_runtime_handles_calibration_before_model_load(self):
        with open("src/python/whisper_dictate/runtime.py", encoding="utf-8") as f:
            script = f.read()

        calibration = script.index("if a.calibrate_mic is not None or a.calibrate_file")
        model_load = script.index("_model = load_stt_model")
        self.assertLess(calibration, model_load)

    def test_cloud_backend_uses_api_device_and_no_local_model_ready_log(self):
        script = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")

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
        script = Path("src/python/whisper_dictate/runtime.py").read_text(encoding="utf-8")

        model_load = script.index("_model = load_stt_model(loaded_model_name, dev, ctype)")
        before = script.rfind("try:", 0, model_load)
        after = script.index("except RuntimeError as e:", model_load)

        self.assertLess(before, model_load)
        self.assertIn('_emit_worker_event("error"', script[model_load:after + 400])
        self.assertIn("startup error", script[model_load:after + 400])
        self.assertIn("raise SystemExit(1)", script[model_load:after + 400])
