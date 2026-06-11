"""Orchestration tests for the live push-to-talk loop (Dictate._stop_and_transcribe).

This is the core product path — captured frames -> transcribe -> post-process ->
inject -> utterance event — and it previously had no test that drove it end to
end. These tests construct a real Dictate (bypassing the heavy __init__ via
object.__new__), feed synthetic PCM, and stub only the boundaries:
  - module: _transcribe_detail (fake model output), is_hallucination, postprocess_text
  - instance: _inject (capture), _record_utterance_event (capture), reload (no-op)
so the real frame handling, skip/hallucination gating, post-process/format wiring
and utterance-event build are exercised.

They run before the runtime.py split refactor so that refactor is provably
behaviour-preserving.
"""
import io
import json
from contextlib import redirect_stderr

from helpers import (
    _capture_stdout,
    _env,
    load_voice_pi_realnp,
    patch,
    types,
    unittest,
)


def _fake_transcribe_result(text):
    return types.SimpleNamespace(
        text=text,
        raw_text=text,
        duration_s=2.0,
        post_boost_dbfs=-20.0,
        raw_dbfs=-30.0,
        peak=0.5,
        gain=2.0,
        noise_dbfs=-70.0,
        snr_db=40.0,
        input_status="ok",
        compute_s=0.4,
        real_time_factor=0.2,
        language="en",
        language_probability=0.99,
        gate="ok",
        segments=[],
        dictionary_terms=[],
        dictionary_replacements=[],
    )


def _passthrough_postprocess(text, _settings=None):
    return types.SimpleNamespace(
        text=text,
        provider="none",
        mode="raw",
        model="",
        latency_ms=0,
        changed=False,
        fallback=False,
        error=None,
        redacted=False,
        redactions=[],
    )


class DictateLoopTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:  # numpy missing
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        # Populate the lazily-loaded module globals (np, SR, _transcribe_detail, ...).
        # The Dictate class lives in vp_dictate now; its methods resolve the
        # transcribe/postprocess helpers from that module's namespace, so the
        # patches below target vp_dictate.
        cls.runtime._load_runtime_modules()
        import importlib
        cls.dictate = importlib.import_module("whisper_dictate.vp_dictate")
        import numpy as np
        cls.np = np

    def _make_dictate(self):
        d = object.__new__(self.dictate.Dictate)
        d.recording = True
        d.release_tail_ms = 0
        d._preview = None
        d._arecord_proc = None
        d._stream = None
        d.audio_ducker = types.SimpleNamespace(enter=lambda: None, exit=lambda: None)
        d._capture_backend = "test"
        d._audio_input_device = "test"
        d._capture_channels = 1
        d.frames = []
        d._record_started = 0.0
        d.stt_backend = "whisper"
        d.parakeet_min_seconds = 1.5
        d.model = object()
        d.lang = "en"
        d.postprocess_settings = None
        d.model_name = "test-model"
        d.device = "cpu"
        d.compute_type = "int8"
        d.model_load_s = 0.0
        d.mode = "print"
        d._last_inject_strategy = None
        d._inject_target_title = None
        d._inject_target_process = None
        d._active_profile_name = None
        # Boundaries we capture instead of executing for real.
        self.injected = []
        self.events = []
        d._inject = self.injected.append
        d._record_utterance_event = self.events.append
        d._reload_live_config_if_changed = lambda: None
        return d

    def _pcm(self, samples):
        return self.np.zeros((samples, 1), dtype=self.np.int16)

    def _run(self, d):
        rt = self.dictate
        with patch.object(rt, "postprocess_text", _passthrough_postprocess), \
                patch.object(rt, "is_hallucination", lambda _t: False), \
                _capture_stdout():
            rt.Dictate._stop_and_transcribe(d)

    def test_full_utterance_is_transcribed_and_injected(self):
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]  # 1.0 s
        with patch.object(self.dictate, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("hej verden")):
            self._run(d)
        self.assertEqual(self.injected, ["hej verden"])
        self.assertEqual(len(self.events), 1)
        self.assertEqual(self.events[0]["text"], "hej verden")
        self.assertEqual(self.events[0]["event"], "utterance")
        self.assertFalse(d.recording)

    def test_too_short_capture_is_skipped(self):
        d = self._make_dictate()
        d.frames = [self._pcm(1000)]  # < 0.3 s -> misfire
        with patch.object(self.dictate, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("ignored")):
            self._run(d)
        self.assertEqual(self.injected, [])
        self.assertEqual(self.events, [])

    def test_hallucination_is_filtered_and_not_injected(self):
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]
        rt = self.dictate
        with patch.object(rt, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("thank you")), \
                patch.object(rt, "postprocess_text", _passthrough_postprocess), \
                patch.object(rt, "is_hallucination", lambda _t: True), \
                _capture_stdout():
            rt.Dictate._stop_and_transcribe(d)
        self.assertEqual(self.injected, [])
        self.assertEqual(self.events, [])

    def test_empty_frames_produce_no_injection(self):
        d = self._make_dictate()
        d.frames = []
        self._run(d)
        self.assertEqual(self.injected, [])
        self.assertEqual(self.events, [])

    # ── no_text event emission ────────────────────────────────────────────────

    def _run_capture_worker_events(self, d):
        """Run _stop_and_transcribe with VOICEPI_WORKER_EVENTS=1 and return
        the parsed list of worker-event payloads emitted to stderr."""
        rt = self.dictate
        stderr_buf = io.StringIO()
        with patch.object(rt, "postprocess_text", _passthrough_postprocess), \
                patch.object(rt, "is_hallucination", lambda _t: False), \
                _capture_stdout(), \
                _env(VOICEPI_WORKER_EVENTS="1"), \
                redirect_stderr(stderr_buf):
            rt.Dictate._stop_and_transcribe(d)
        events = []
        for line in stderr_buf.getvalue().splitlines():
            prefix = "[worker-event] "
            if line.startswith(prefix):
                events.append(json.loads(line[len(prefix):]))
        return events

    def test_no_frames_emits_no_text_no_audio(self):
        d = self._make_dictate()
        d.frames = []
        events = self._run_capture_worker_events(d)
        no_text = [e for e in events if e.get("state") == "no_text"]
        self.assertEqual(len(no_text), 1, f"expected one no_text event; got {events}")
        self.assertEqual(no_text[0]["reason"], "no_audio")

    def test_too_short_clip_emits_no_text_too_short(self):
        d = self._make_dictate()
        d.frames = [self._pcm(1000)]  # < 0.3 s → misfire
        with patch.object(self.dictate, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("ignored")):
            events = self._run_capture_worker_events(d)
        no_text = [e for e in events if e.get("state") == "no_text"]
        self.assertEqual(len(no_text), 1, f"expected one no_text event; got {events}")
        self.assertEqual(no_text[0]["reason"], "too_short")
        # recording_s must be reported for too_short (so the user sees how long they held)
        self.assertIn("recording_s", no_text[0])

    def test_no_text_not_emitted_on_successful_utterance(self):
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]
        with patch.object(self.dictate, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("hello")):
            events = self._run_capture_worker_events(d)
        no_text = [e for e in events if e.get("state") == "no_text"]
        self.assertEqual(no_text, [], f"unexpected no_text events on success: {events}")

    # ── chord-cancel epoch guard (Finding 2 / 4d) ─────────────────────────────

    def test_cancel_matching_epoch_discards(self):
        # A cancel dispatched for the CURRENT recording generation discards the
        # in-flight audio (no transcribe/inject).
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]
        d._record_epoch = 7
        d._discard_recording = False
        with patch.object(self.dictate, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("should not")), \
                patch.object(self.dictate, "postprocess_text",
                             _passthrough_postprocess), \
                patch.object(self.dictate, "is_hallucination", lambda _t: False), \
                _capture_stdout():
            self.dictate.Dictate._cancel_and_discard(d, 7)
        self.assertEqual(self.injected, [])      # discarded, nothing injected
        self.assertEqual(d.frames, [])           # audio dropped
        self.assertFalse(d.recording)

    def test_stale_cancel_for_old_epoch_noops(self):
        # Finding 2/4d: a cancel dispatched for epoch N must NOT discard a NEW
        # recording (epoch N+1) — release + re-press happened before the daemon
        # thread ran. The new clip is preserved (recording stays active).
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]
        d._record_epoch = 8            # a NEW recording is active
        d._discard_recording = False
        with _capture_stdout():
            self.dictate.Dictate._cancel_and_discard(d, 7)  # stale: epoch 7
        self.assertTrue(d.recording)            # untouched
        self.assertFalse(d._discard_recording)  # never armed the discard
        self.assertEqual(self.injected, [])     # and never transcribed

    # ── Part B: native-rate capture is resampled at consumption ───────────────

    def test_native_rate_buffer_is_resampled_to_16k_for_transcription(self):
        # A 48k-native capture (e.g. a Yeti opened after a 16k open was rejected)
        # must reach the model resampled to 16k: 1.0 s of 48k audio (48000
        # frames) becomes ~16000 samples, not 48000.
        d = self._make_dictate()
        d._capture_rate = 48000
        d.frames = [self._pcm(48000)]  # 1.0 s at 48k
        seen = {}

        def _capture_pcm(_model, pcm, _lang):
            seen["len"] = len(pcm)
            return _fake_transcribe_result("nativ lyd")

        with patch.object(self.dictate, "_transcribe_detail", _capture_pcm):
            self._run(d)
        self.assertEqual(self.injected, ["nativ lyd"])
        self.assertTrue(abs(seen["len"] - 16000) <= 1,
                        f"expected ~16000 samples at 16k, got {seen['len']}")

    def test_16k_native_buffer_is_not_resampled(self):
        # capture_rate == SR is a no-op: the model sees exactly the captured
        # samples (16000), unchanged from current 16k-device behaviour.
        d = self._make_dictate()
        d._capture_rate = 16000
        d.frames = [self._pcm(16000)]
        seen = {}

        def _capture_pcm(_model, pcm, _lang):
            seen["len"] = len(pcm)
            return _fake_transcribe_result("seksten kilo")

        with patch.object(self.dictate, "_transcribe_detail", _capture_pcm):
            self._run(d)
        self.assertEqual(seen["len"], 16000)

    def test_missing_capture_rate_defaults_to_16k_no_resample(self):
        # An object.__new__ instance without _capture_rate (defensive getattr)
        # must behave as 16k-native — no crash, no resample.
        d = self._make_dictate()
        # Intentionally do NOT set d._capture_rate.
        d.frames = [self._pcm(16000)]
        seen = {}

        def _capture_pcm(_model, pcm, _lang):
            seen["len"] = len(pcm)
            return _fake_transcribe_result("standard")

        with patch.object(self.dictate, "_transcribe_detail", _capture_pcm):
            self._run(d)
        self.assertEqual(seen["len"], 16000)


class StartCrashRecoveryTests(unittest.TestCase):
    """Part A: a capture open/start failure must NOT escape Dictate._start (it
    runs on the pynput on_press listener thread) — it is caught, an actionable
    error event is emitted, and the session stays idle + usable for a retry."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.dictate = importlib.import_module("whisper_dictate.vp_dictate")
        cls.capture = importlib.import_module("whisper_dictate.vp_capture")

    def _make_dictate(self):
        d = object.__new__(self.dictate.Dictate)
        d.recording = False
        d.frames = []
        d._first_audio_event = self.capture.threading.Event()
        d._first_audio_at = 0.0
        d._last_audio_level_event = 0.0
        d._record_epoch = 0
        d._record_started = 0.0
        d._record_keydown_at = 0.0
        d._capture_backend = ""
        d._audio_input_device = "Microphone (Yeti Classic)"
        d._capture_channels = 1
        d._stream = None
        d._arecord_proc = None
        d.audio_ducker = types.SimpleNamespace(
            enter=lambda: None, exit=lambda: None)
        d._effective_config = {}
        # No-op the orchestration boundaries the test doesn't exercise.
        d._reload_live_config_if_changed = lambda: None
        d._capture_target_window = lambda: None
        d._profiled_config = lambda cfg: d._effective_config
        d._start_preview = lambda: None
        return d

    def _run_start(self, d):
        """Run Dictate._start capturing worker events; return parsed payloads."""
        rt = self.dictate
        stderr_buf = io.StringIO()
        with patch.object(rt, "effective_config", lambda: {}), \
                patch.object(rt, "play_cue", lambda *_a, **_k: None), \
                patch.object(self.capture, "_arecord_device", lambda: None), \
                _env(VOICEPI_WORKER_EVENTS="1"), \
                _capture_stdout(), \
                redirect_stderr(stderr_buf):
            rt.Dictate._start(d)
        events = []
        for line in stderr_buf.getvalue().splitlines():
            if line.startswith("[worker-event] "):
                events.append(json.loads(line[len("[worker-event] "):]))
        return events

    def test_start_failure_does_not_propagate_and_emits_error_event(self):
        d = self._make_dictate()
        # _start_sounddevice raises the Yeti's start-time PortAudio error.
        d._start_sounddevice = lambda: (_ for _ in ()).throw(
            RuntimeError("Error starting stream: AUDCLNT_E_UNSUPPORTED_FORMAT"))

        events = self._run_start(d)  # must NOT raise

        # (1) no exception propagated (we got here). (2) an error event names
        # the device and carries an actionable message.
        errors = [e for e in events if e.get("state") == "error"]
        self.assertEqual(len(errors), 1, f"expected one error event; got {events!r}")
        self.assertIn("Yeti", errors[0].get("audio_device", ""))
        self.assertIn("Yeti", errors[0].get("error", ""))
        self.assertIn("microphone", errors[0].get("error", "").lower())
        # (3) session is idle + usable again (recording reset, ready emitted).
        self.assertFalse(d.recording)
        self.assertTrue(any(e.get("state") == "ready" for e in events))

    def test_session_can_attempt_start_again_after_a_failure(self):
        d = self._make_dictate()
        attempts = {"n": 0}

        def _flaky():
            attempts["n"] += 1
            if attempts["n"] == 1:
                raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
            # Second attempt succeeds.
            d._capture_backend = "sounddevice"
            return "sounddevice", "Microphone (Yeti Classic)"

        d._start_sounddevice = _flaky

        self._run_start(d)            # first press fails, recovers
        self.assertFalse(d.recording)
        # Second press: the worker is still alive and can start a recording.
        self._run_start(d)
        self.assertEqual(attempts["n"], 2)
        self.assertTrue(d.recording)  # second attempt is now recording


if __name__ == "__main__":
    unittest.main()
