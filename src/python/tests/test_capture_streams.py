"""Capture teardown + metering tests for the RECORDING loop (Dictate capture).

These drive the stop/close path and the live audio-level metering with a FAKE
audio backend (stubbed subprocess.Popen / sounddevice), as in test_capture.py.
The arecord reader and the arecord/sounddevice start paths live in
test_capture.py.

Covered:
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

    def wait(self, timeout=None):
        self.waited = True


class StopCaptureStreamsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import importlib
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")

    def test_stop_capture_streams_terminates_arecord_proc(self):
        rt = self.runtime
        proc = _FakeProc([])
        target = types.SimpleNamespace(_arecord_proc=proc, _stream=None, frames=[])

        rt.CaptureMixin._stop_capture_streams(target)

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
        target = types.SimpleNamespace(_arecord_proc=None, _stream=stream, frames=[])

        rt.CaptureMixin._stop_capture_streams(target)

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
        target = types.SimpleNamespace(_arecord_proc=proc, _stream=stream, frames=[])

        rt.CaptureMixin._stop_capture_streams(target)

        self.assertTrue(proc.terminated)
        self.assertTrue(stream.stopped and stream.closed)
        self.assertIsNone(target._arecord_proc)
        self.assertIsNone(target._stream)

    def test_stop_capture_streams_is_noop_when_idle(self):
        rt = self.runtime
        target = types.SimpleNamespace(_arecord_proc=None, _stream=None, frames=[])

        # Must not raise when nothing is active.
        rt.CaptureMixin._stop_capture_streams(target)

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
        # The capture methods + their globals (np, SR, _ARECORD_DEVICE,
        # subprocess, threading) live in vp_capture's CaptureMixin; drive them
        # through that module so patched globals resolve where the methods read.
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")
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
                rt.CaptureMixin._emit_audio_level(target, pcm)

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
                rt.CaptureMixin._emit_audio_level(target, pcm)

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
                rt.CaptureMixin._emit_audio_level(target, pcm)

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
        # The capture methods + their globals (np, SR, _ARECORD_DEVICE,
        # subprocess, threading) live in vp_capture's CaptureMixin; drive them
        # through that module so patched globals resolve where the methods read.
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def test_recording_seconds_uses_monotonic_start_when_available(self):
        rt = self.runtime
        np = self.np
        start = rt.time.monotonic() - 5.0
        target = types.SimpleNamespace(_record_started=start)
        pcm = np.zeros((16000, 1), dtype=np.int16)  # 1 s by sample count

        secs = rt.CaptureMixin._recording_seconds(target, pcm)

        # Wall-clock based: ~5 s, not the 1 s implied by sample count.
        self.assertGreaterEqual(secs, 4.5)
        self.assertLess(secs, 7.0)

    def test_recording_seconds_falls_back_to_sample_count(self):
        rt = self.runtime
        np = self.np
        target = types.SimpleNamespace(_record_started=0.0)
        pcm = np.zeros((rt.SR * 2, 1), dtype=np.int16)  # exactly 2 s of samples

        secs = rt.CaptureMixin._recording_seconds(target, pcm)

        self.assertAlmostEqual(secs, 2.0, places=3)


if __name__ == "__main__":
    unittest.main()
