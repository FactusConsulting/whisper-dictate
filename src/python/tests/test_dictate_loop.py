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
from helpers import (
    _capture_stdout,
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
        cls.runtime._load_runtime_modules()
        import numpy as np
        cls.np = np

    def _make_dictate(self):
        d = object.__new__(self.runtime.Dictate)
        d.recording = True
        d.release_tail_ms = 0
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
        rt = self.runtime
        with patch.object(rt, "postprocess_text", _passthrough_postprocess), \
                patch.object(rt, "is_hallucination", lambda _t: False), \
                _capture_stdout():
            rt.Dictate._stop_and_transcribe(d)

    def test_full_utterance_is_transcribed_and_injected(self):
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]  # 1.0 s
        with patch.object(self.runtime, "_transcribe_detail",
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
        with patch.object(self.runtime, "_transcribe_detail",
                          lambda *_a, **_k: _fake_transcribe_result("ignored")):
            self._run(d)
        self.assertEqual(self.injected, [])
        self.assertEqual(self.events, [])

    def test_hallucination_is_filtered_and_not_injected(self):
        d = self._make_dictate()
        d.frames = [self._pcm(16000)]
        rt = self.runtime
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


if __name__ == "__main__":
    unittest.main()
