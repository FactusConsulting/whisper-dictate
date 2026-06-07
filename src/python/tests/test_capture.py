"""Capture-orchestration tests for the RECORDING path (Dictate audio capture).

These drive the live capture plumbing with a FAKE audio backend (no real
sounddevice/arecord device touched). As in test_dictate_loop.py, a real
Dictate is built via object.__new__ and only the attributes each code path
reads are set; the OS/audio boundaries (subprocess.Popen, threading.Thread,
sounddevice) are stubbed so frames accumulate and streams stop cleanly.

Covered:
  - _arecord_reader: reads S16_LE chunks from a fake proc into self.frames,
    sets the first-audio event, stops when recording flips off / EOF.
  - _start_arecord: backend/device/channel selection + reader thread wiring.
  - _start_sounddevice: device-name + channel selection (extends the
    fallback test already in test_audio.py).
  - _stop_capture_streams: cleanly stops/closes a fake stream and a fake
    arecord proc.
  - _emit_audio_level: 0.12s throttle + metered worker event payload.
  - _recording_seconds: monotonic start vs sample-count fallback.
"""
from helpers import (
    _env,
    io,
    json,
    load_voice_pi_realnp,
    patch,
    redirect_stderr,
    types,
    unittest,
)


class _FakeStdout:
    """Minimal arecord stdout: hands back queued byte chunks then EOF."""

    def __init__(self, chunks):
        self._chunks = list(chunks)

    def read(self, _n):
        if self._chunks:
            return self._chunks.pop(0)
        return b""


class _FakeProc:
    def __init__(self, chunks):
        self.stdout = _FakeStdout(chunks)
        self.terminated = False
        self.waited = False

    def terminate(self):
        self.terminated = True

    def wait(self):
        self.waited = True


class ArecordReaderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:  # numpy missing
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # Dictate + its capture globals (np, SR, _ARECORD_DEVICE, subprocess,
        # threading) now live in vp_dictate; drive the class through it.
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")
        import numpy as np
        cls.np = np

    def _reader_target(self):
        np = self.np
        return types.SimpleNamespace(
            recording=True,
            frames=[],
            _first_audio_event=self.runtime.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _chunk: None,
        )

    def test_arecord_reader_accumulates_int16_frames_and_sets_first_audio(self):
        np = self.np
        # Two ~125ms chunks of S16 mono. Reader reshapes to (-1, 1).
        c0 = np.full(2000, 100, dtype=np.int16).tobytes()
        c1 = np.full(2000, 200, dtype=np.int16).tobytes()
        proc = _FakeProc([c0, c1])
        target = self._reader_target()

        self.runtime.Dictate._arecord_reader(target, proc)

        self.assertEqual(len(target.frames), 2)
        self.assertEqual(target.frames[0].shape, (2000, 1))
        self.assertEqual(target.frames[0].dtype, np.int16)
        self.assertEqual(int(target.frames[0][0, 0]), 100)
        self.assertEqual(int(target.frames[1][0, 0]), 200)
        # First-audio event/time are set on the very first chunk.
        self.assertTrue(target._first_audio_event.is_set())
        self.assertGreater(target._first_audio_at, 0.0)
        self.assertEqual(target._record_started, target._first_audio_at)

    def test_arecord_reader_stops_on_eof(self):
        np = self.np
        proc = _FakeProc([np.zeros(2000, dtype=np.int16).tobytes()])
        target = self._reader_target()

        # recording stays True, but the fake proc returns b"" after one chunk,
        # so the loop must break on EOF rather than spin forever.
        self.runtime.Dictate._arecord_reader(target, proc)

        self.assertEqual(len(target.frames), 1)

    def test_arecord_reader_stops_when_recording_flag_clears(self):
        np = self.np
        # Plenty of chunks queued; reader must stop after the flag clears.
        chunks = [np.zeros(2000, dtype=np.int16).tobytes() for _ in range(50)]
        proc = _FakeProc(chunks)
        target = self._reader_target()

        emitted = {"n": 0}

        def _flip(_chunk):
            emitted["n"] += 1
            if emitted["n"] >= 3:
                target.recording = False

        target._emit_audio_level = _flip

        self.runtime.Dictate._arecord_reader(target, proc)

        # Loop re-checks self.recording at the top: after the 3rd emit flips
        # the flag, no further chunk is read. So exactly 3 frames.
        self.assertEqual(len(target.frames), 3)

    def test_arecord_reader_emits_audio_level_per_chunk(self):
        np = self.np
        proc = _FakeProc([np.zeros(2000, dtype=np.int16).tobytes()])
        seen = []
        target = self._reader_target()
        target._emit_audio_level = seen.append

        self.runtime.Dictate._arecord_reader(target, proc)

        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0].shape, (2000, 1))


class StartArecordTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # Dictate + its capture globals (np, SR, _ARECORD_DEVICE, subprocess,
        # threading) now live in vp_dictate; drive the class through it.
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")

    def test_start_arecord_sets_backend_device_and_spawns_reader_thread(self):
        rt = self.runtime
        np = __import__("numpy")
        chunk = np.zeros(2000, dtype=np.int16).tobytes()
        fake_proc = _FakeProc([chunk])

        popen_calls = []

        def fake_popen(cmd, **kwargs):
            popen_calls.append((cmd, kwargs))
            return fake_proc

        thread_calls = []

        class FakeThread:
            def __init__(self, target=None, args=(), daemon=None):
                thread_calls.append((target, args, daemon))
                self._target = target
                self._args = args

            def start(self):
                # Run synchronously so we can assert frames accumulate.
                self._target(*self._args)

        target = types.SimpleNamespace(
            recording=True,
            frames=[],
            _arecord_proc=None,
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _first_audio_event=rt.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _c: None,
        )
        # The reader thread target calls self._arecord_reader; bind the real
        # method so the synchronous FakeThread fills frames for real.
        target._arecord_reader = lambda proc: rt.Dictate._arecord_reader(target, proc)

        with patch.object(rt, "_ARECORD_DEVICE", "pipewire"), \
                patch.object(rt.subprocess, "Popen", fake_popen), \
                patch.object(rt.threading, "Thread", FakeThread):
            backend, device = rt.Dictate._start_arecord(target)

        self.assertEqual(backend, "arecord")
        self.assertEqual(device, "arecord -D pipewire")
        self.assertEqual(target._capture_backend, "arecord")
        self.assertEqual(target._capture_channels, 1)
        self.assertIs(target._arecord_proc, fake_proc)
        # arecord invoked with the expected device + S16_LE mono 16k flags.
        cmd = popen_calls[0][0]
        self.assertEqual(cmd[:3], ["arecord", "-D", "pipewire"])
        self.assertIn("S16_LE", cmd)
        self.assertIn(str(rt.SR), cmd)
        # Reader thread is a daemon and (run synchronously here) filled frames.
        self.assertEqual(len(thread_calls), 1)
        self.assertTrue(thread_calls[0][2])  # daemon=True
        self.assertEqual(len(target.frames), 1)


class StartSounddeviceTests(unittest.TestCase):
    """Extends the low-latency-fallback test in test_audio.py with the
    happy path and the channel-candidate fallback ordering."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # Dictate + its capture globals (np, SR, _ARECORD_DEVICE, subprocess,
        # threading) now live in vp_dictate; drive the class through it.
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")

    def _fake_target(self):
        return types.SimpleNamespace(
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _stream=None,
            _cb=lambda *_a: None,
        )

    def test_start_sounddevice_uses_first_working_channel_count(self):
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (1, 2)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Studio Mic",
                "max_input_channels": 2,
            },
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            backend, device = rt.Dictate._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertEqual(device, "Studio Mic")
        # max_input_channels=2 -> first candidate is 2 and it works immediately.
        self.assertEqual(target._capture_channels, 2)
        self.assertEqual(opened[0]["channels"], 2)
        self.assertTrue(target._stream.started)

    def test_start_sounddevice_falls_back_to_fewer_channels(self):
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The hardware only accepts mono; reject anything wider.
                if kwargs["channels"] != 1:
                    raise RuntimeError("invalid channel count")
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (4, 5)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Mono Cam",
                "max_input_channels": 4,
            },
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            backend, device = rt.Dictate._start_sounddevice(target)

        self.assertEqual(target._capture_channels, 1)
        self.assertTrue(target._stream.started)
        # Candidates for max=4 are [4, 2, 1]; only 1 succeeds.
        self.assertEqual([k["channels"] for k in opened][-1], 1)
        self.assertIn(4, [k["channels"] for k in opened])

    def test_start_sounddevice_reraises_last_error_when_all_fail(self):
        rt = self.runtime
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                raise RuntimeError("device busy")

        class Default:
            device = (0, 0)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Busy", "max_input_channels": 1},
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            with self.assertRaises(RuntimeError) as ctx:
                rt.Dictate._start_sounddevice(target)
        self.assertIn("device busy", str(ctx.exception))
        self.assertIsNone(target._stream)


class StopCaptureStreamsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import importlib
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")

    def test_stop_capture_streams_terminates_arecord_proc(self):
        rt = self.runtime
        proc = _FakeProc([])
        target = types.SimpleNamespace(_arecord_proc=proc, _stream=None)

        rt.Dictate._stop_capture_streams(target)

        self.assertTrue(proc.terminated)
        self.assertTrue(proc.waited)
        self.assertIsNone(target._arecord_proc)

    def test_stop_capture_streams_stops_and_closes_sounddevice_stream(self):
        rt = self.runtime

        class Stream:
            def __init__(self):
                self.stopped = False
                self.closed = False

            def stop(self):
                self.stopped = True

            def close(self):
                self.closed = True

        stream = Stream()
        target = types.SimpleNamespace(_arecord_proc=None, _stream=stream)

        rt.Dictate._stop_capture_streams(target)

        self.assertTrue(stream.stopped)
        self.assertTrue(stream.closed)
        self.assertIsNone(target._stream)

    def test_stop_capture_streams_handles_both_backends(self):
        rt = self.runtime
        proc = _FakeProc([])

        class Stream:
            stopped = False
            closed = False

            def stop(self):
                self.stopped = True

            def close(self):
                self.closed = True

        stream = Stream()
        target = types.SimpleNamespace(_arecord_proc=proc, _stream=stream)

        rt.Dictate._stop_capture_streams(target)

        self.assertTrue(proc.terminated)
        self.assertTrue(stream.stopped and stream.closed)
        self.assertIsNone(target._arecord_proc)
        self.assertIsNone(target._stream)

    def test_stop_capture_streams_is_noop_when_idle(self):
        rt = self.runtime
        target = types.SimpleNamespace(_arecord_proc=None, _stream=None)

        # Must not raise when nothing is active.
        rt.Dictate._stop_capture_streams(target)

        self.assertIsNone(target._arecord_proc)
        self.assertIsNone(target._stream)


class EmitAudioLevelTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # Dictate + its capture globals (np, SR, _ARECORD_DEVICE, subprocess,
        # threading) now live in vp_dictate; drive the class through it.
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")
        import numpy as np
        cls.np = np

    def _target(self, last_event=0.0):
        return types.SimpleNamespace(
            _last_audio_level_event=last_event,
            _capture_backend="sounddevice",
            _audio_input_device="Studio Mic",
            _capture_channels=1,
        )

    def test_emit_audio_level_throttles_within_120ms_window(self):
        rt = self.runtime
        np = self.np
        # Pin the clock so the throttle window is deterministic (not wall-clock).
        target = self._target(last_event=1000.0)
        pcm = np.zeros((2000, 1), dtype=np.int16)

        with _env(VOICEPI_WORKER_EVENTS="1"), \
                patch.object(rt.time, "monotonic", lambda: 1000.05):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                rt.Dictate._emit_audio_level(target, pcm)

        self.assertEqual(stderr.getvalue(), "")
        # Throttled (0.05s < 0.12s gate): timestamp not advanced.
        self.assertEqual(target._last_audio_level_event, 1000.0)

    def test_emit_audio_level_emits_metered_worker_event(self):
        rt = self.runtime
        np = self.np
        target = self._target(last_event=0.0)
        # ~0.1 amplitude -> roughly -20 dBFS, a clearly visible meter level.
        pcm = (np.full((2000, 1), 0.1, dtype=np.float32) * 32767).astype(np.int16)

        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                rt.Dictate._emit_audio_level(target, pcm)

        line = stderr.getvalue().strip()
        self.assertTrue(line.startswith("[worker-event] "))
        payload = json.loads(line.removeprefix("[worker-event] "))
        self.assertEqual(payload["event"], "audio")
        self.assertEqual(payload["state"], "recording")
        self.assertEqual(payload["capture_backend"], "sounddevice")
        self.assertEqual(payload["audio_device"], "Studio Mic")
        self.assertEqual(payload["capture_channels"], 1)
        self.assertAlmostEqual(payload["raw_dbfs"], -20.0, places=0)
        self.assertGreater(payload["level"], 0.7)
        # Emission advanced the throttle timestamp.
        self.assertGreater(target._last_audio_level_event, 0.0)

    def test_emit_audio_level_reports_silence_as_low_level(self):
        rt = self.runtime
        np = self.np
        target = self._target(last_event=0.0)
        pcm = np.zeros((2000, 1), dtype=np.int16)

        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                rt.Dictate._emit_audio_level(target, pcm)

        payload = json.loads(
            stderr.getvalue().strip().removeprefix("[worker-event] "))
        self.assertEqual(payload["level"], 0.0)
        self.assertEqual(payload["peak"], 0.0)


class RecordingSecondsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # Dictate + its capture globals (np, SR, _ARECORD_DEVICE, subprocess,
        # threading) now live in vp_dictate; drive the class through it.
        cls.runtime = importlib.import_module("whisper_dictate.vp_dictate")
        import numpy as np
        cls.np = np

    def test_recording_seconds_uses_monotonic_start_when_available(self):
        rt = self.runtime
        np = self.np
        start = rt.time.monotonic() - 5.0
        target = types.SimpleNamespace(_record_started=start)
        pcm = np.zeros((16000, 1), dtype=np.int16)  # 1 s by sample count

        secs = rt.Dictate._recording_seconds(target, pcm)

        # Wall-clock based: ~5 s, not the 1 s implied by sample count.
        self.assertGreaterEqual(secs, 4.5)
        self.assertLess(secs, 7.0)

    def test_recording_seconds_falls_back_to_sample_count(self):
        rt = self.runtime
        np = self.np
        target = types.SimpleNamespace(_record_started=0.0)
        pcm = np.zeros((rt.SR * 2, 1), dtype=np.int16)  # exactly 2 s of samples

        secs = rt.Dictate._recording_seconds(target, pcm)

        self.assertAlmostEqual(secs, 2.0, places=3)


if __name__ == "__main__":
    unittest.main()
