"""Unit tests for the live partial-transcription preview (vp_preview, issue #16).

The preview is a background loop that periodically transcribes the in-progress
recording buffer and emits a ``state="preview"`` worker event so the UI can show
the sentence growing. It is strictly display-only: the FINAL transcription at
key release is unchanged.

These tests drive ``PreviewEngine._tick`` directly (no real thread / sleep) and
stub only the boundaries:
  - module: ``transcribe_preview`` (fake decode), ``TRANSCRIBE_LOCK`` (busy/free),
    ``_emit_worker_event`` (capture)
so the real interval gating, lock-busy skip, backend/disabled gating and event
truncation are exercised. Mirrors the SimpleNamespace harness style of
test_dictate_loop.py.
"""
import threading

from helpers import (
    load_voice_pi_realnp,
    patch,
    types,
    unittest,
)


class _CapturingLock:
    """A lock stub that records acquire(blocking=False) calls and can be 'busy'."""

    def __init__(self, *, busy=False):
        self._busy = busy
        self.acquire_calls = 0
        self.release_calls = 0

    def acquire(self, blocking=True):
        self.acquire_calls += 1
        return not self._busy

    def release(self):
        self.release_calls += 1


class PreviewEngineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:  # numpy missing
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.preview = importlib.import_module("whisper_dictate.vp_preview")
        import numpy as np
        cls.np = np
        cls.SR = 16000

    def _owner(self, *, frames=None, recording=True, lang="en"):
        return types.SimpleNamespace(
            frames=frames if frames is not None else [],
            recording=recording,
            model=object(),
            lang=lang,
        )

    def _pcm_seconds(self, seconds):
        return self.np.zeros((int(self.SR * seconds), 1), dtype=self.np.int16)

    def _make_engine(self, owner, interval_s=3.0):
        return self.preview.PreviewEngine(owner, interval_s)

    def _events(self):
        captured = []

        def _emit(event, **fields):
            captured.append({"event": event, **fields})

        return captured, _emit

    # ── interval gating ───────────────────────────────────────────────────────

    def test_no_new_audio_does_not_transcribe(self):
        # Two ticks with the same buffer: the second has no FRESH audio past the
        # first preview, so it must NOT transcribe or emit again.
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        transcribe_calls = []

        def _fake_transcribe(model, pcm, lang):
            transcribe_calls.append(len(pcm))
            return "hello world"

        with patch.object(self.preview, "transcribe_preview", _fake_transcribe), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()  # first preview → transcribes
            engine._tick()  # no new audio → skipped

        self.assertEqual(len(transcribe_calls), 1, "second tick re-transcribed unchanged buffer")
        self.assertEqual(len([e for e in events if e.get("state") == "preview"]), 1)

    # ── sliding-window cap on the preview decode input ─────────────────────────

    def test_long_buffer_is_capped_to_window(self):
        # A buffer far longer than PREVIEW_MAX_AUDIO_S must hand transcribe_preview
        # only the most recent window of samples (bounded cost), NOT the whole
        # buffer-so-far. Capture the array the fake decode receives and assert its
        # length is <= PREVIEW_MAX_AUDIO_S * SR.
        window_s = self.preview.PREVIEW_MAX_AUDIO_S
        owner = self._owner(frames=[self._pcm_seconds(window_s * 4)])  # 60s vs 15s
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        seen = []

        def _fake_transcribe(model, pcm, lang):
            seen.append(len(pcm))
            return "rolling window"

        with patch.object(self.preview, "transcribe_preview", _fake_transcribe), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        max_samples = int(window_s * self.SR)
        self.assertEqual(len(seen), 1)
        self.assertLessEqual(seen[0], max_samples,
                             "preview decode input exceeded the sliding window")
        self.assertEqual(seen[0], max_samples,
                         "a far-longer buffer should fill exactly the window")

    def test_short_buffer_passed_in_full(self):
        # A buffer SHORTER than the window is decoded in full (no trimming) —
        # unchanged behavior for short utterances.
        window_s = self.preview.PREVIEW_MAX_AUDIO_S
        short_s = window_s / 3.0  # 5s < 15s window
        owner = self._owner(frames=[self._pcm_seconds(short_s)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        seen = []

        def _fake_transcribe(model, pcm, lang):
            seen.append(len(pcm))
            return "short"

        with patch.object(self.preview, "transcribe_preview", _fake_transcribe), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0], int(self.SR * short_s),
                         "short buffer must be passed in full (no trimming)")

    def test_window_does_not_freeze_gate_or_recording_s(self):
        # Once the buffer EXCEEDS the window the decode length is pinned to the
        # window, but the fresh-audio gate / recording_s track the TOTAL captured
        # length — so a second tick with another window+ of fresh audio still
        # previews, and recording_s reflects real elapsed time (not the cap).
        window_s = self.preview.PREVIEW_MAX_AUDIO_S
        owner = self._owner(frames=[self._pcm_seconds(window_s + 2)])  # 17s
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        seen = []

        def _fake_transcribe(model, pcm, lang):
            seen.append(len(pcm))
            return "still going"

        with patch.object(self.preview, "transcribe_preview", _fake_transcribe), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()  # first preview at 17s
            owner.frames = [self._pcm_seconds(window_s * 2 + 4)]  # grow to 34s
            engine._tick()  # plenty of fresh audio → must preview again

        self.assertEqual(len(seen), 2, "gate froze: second tick did not preview")
        preview_events = [e for e in events if e.get("state") == "preview"]
        self.assertEqual(len(preview_events), 2)
        # recording_s tracks TOTAL elapsed audio, not the capped decode length.
        self.assertAlmostEqual(preview_events[0]["recording_s"], window_s + 2, places=1)
        self.assertAlmostEqual(preview_events[1]["recording_s"], window_s * 2 + 4, places=1)

    def test_below_min_new_audio_skips(self):
        # A buffer under the minimum fresh-audio threshold (1.5s) never previews.
        owner = self._owner(frames=[self._pcm_seconds(1.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()

        with patch.object(self.preview, "transcribe_preview",
                          lambda *_a, **_k: "nope"), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(lock.acquire_calls, 0, "should not even reach the lock")
        self.assertEqual(events, [])

    # ── lock-busy skip ────────────────────────────────────────────────────────

    def test_lock_busy_skips_without_transcribing(self):
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        busy_lock = _CapturingLock(busy=True)
        transcribe_calls = []

        with patch.object(self.preview, "transcribe_preview",
                          lambda *a, **k: transcribe_calls.append(1) or "x"), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", busy_lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(busy_lock.acquire_calls, 1)
        self.assertEqual(busy_lock.release_calls, 0, "must not release a lock it never got")
        self.assertEqual(transcribe_calls, [], "must not transcribe while lock is busy")
        self.assertEqual(events, [])

    def test_lock_released_after_successful_preview(self):
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()

        with patch.object(self.preview, "transcribe_preview",
                          lambda *_a, **_k: "released path"), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(lock.acquire_calls, 1)
        self.assertEqual(lock.release_calls, 1, "lock must be released after a preview")

    # ── event emission + truncation ───────────────────────────────────────────

    def test_event_emitted_with_truncated_text_and_recording_s(self):
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        long_text = "word " * 200  # ~1000 chars → must be truncated to the cap

        with patch.object(self.preview, "transcribe_preview",
                          lambda *_a, **_k: long_text), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        preview_events = [e for e in events if e.get("state") == "preview"]
        self.assertEqual(len(preview_events), 1, f"expected one preview event; got {events}")
        ev = preview_events[0]
        self.assertEqual(ev["event"], "status")
        self.assertLessEqual(len(ev["text_preview"]), self.preview.PREVIEW_TEXT_CHARS)
        self.assertIn("recording_s", ev)
        self.assertAlmostEqual(ev["recording_s"], 3.0, places=1)

    def test_empty_transcription_emits_nothing(self):
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()

        with patch.object(self.preview, "transcribe_preview", lambda *_a, **_k: ""), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(events, [], "empty preview text must not emit an event")
        self.assertEqual(lock.release_calls, 1, "lock still released on empty text")

    def test_stopped_recording_during_preview_emits_nothing(self):
        # If recording stopped while the (locked) decode ran, drop the result.
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()

        def _transcribe_then_stop(*_a, **_k):
            owner.recording = False
            return "too late"

        with patch.object(self.preview, "transcribe_preview", _transcribe_then_stop), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(events, [])

    # ── error swallowing ──────────────────────────────────────────────────────

    def test_transcribe_error_is_swallowed_and_logged_once(self):
        owner = self._owner(frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()

        def _boom(*_a, **_k):
            raise RuntimeError("decode blew up")

        import io
        import contextlib
        buf = io.StringIO()
        with patch.object(self.preview, "transcribe_preview", _boom), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit), \
                contextlib.redirect_stdout(buf):
            engine._tick()
            # advance the buffer so the second tick passes the new-audio gate,
            # then fail again — but only ONE "[preview] failed" line is logged.
            owner.frames = [self._pcm_seconds(6.0)]
            engine._tick()

        self.assertEqual(events, [])
        self.assertEqual(buf.getvalue().count("[preview] failed"), 1)
        # The lock must still be released after an exception inside the critical
        # section (try/finally), so the final pass is never blocked.
        self.assertEqual(lock.release_calls, lock.acquire_calls)

    # ── gating helper ─────────────────────────────────────────────────────────

    def test_preview_enabled_gating(self):
        import os
        enabled = self.preview.preview_enabled
        # Local whisper backend with a positive interval → enabled (env-var
        # clean so the rust-shellout gate doesn't fire).
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("VOICEPI_TRANSCRIBE_BACKEND", None)
            self.assertTrue(enabled(3.0, "whisper"))
            # Disabled when interval is 0 (or negative).
            self.assertFalse(enabled(0.0, "whisper"))
            self.assertFalse(enabled(-1.0, "whisper"))
            # Cloud (paid API) and Parakeet are never previewed.
            self.assertFalse(enabled(3.0, "openai"))
            self.assertFalse(enabled(3.0, "parakeet"))
            # Case / whitespace tolerant.
            self.assertTrue(enabled(3.0, "  Whisper "))

    def test_preview_disabled_for_rust_shell_backend(self):
        """``VOICEPI_TRANSCRIBE_BACKEND=rust`` swaps in a one-shot subprocess
        wrapper that reloads the GGML model on every call — running the live
        preview against it would spawn a helper + reload the model every few
        seconds AND hold ``TRANSCRIBE_LOCK`` long enough to delay the final
        pass. Previews must stay disabled until streaming Rust lands."""
        import os
        enabled = self.preview.preview_enabled
        with patch.dict(os.environ,
                        {"VOICEPI_TRANSCRIBE_BACKEND": "rust"}):
            self.assertFalse(enabled(3.0, "whisper"))
            self.assertFalse(enabled(3.0, "Whisper"))
        # Any other value of the env var leaves preview enabled.
        with patch.dict(os.environ,
                        {"VOICEPI_TRANSCRIBE_BACKEND": "python"}):
            self.assertTrue(enabled(3.0, "whisper"))

    def test_start_noop_when_interval_zero(self):
        owner = self._owner()
        engine = self._make_engine(owner, interval_s=0.0)
        engine.start()
        self.assertIsNone(engine._thread, "interval=0 must not start a thread")

    def test_run_exits_promptly_when_recording_stops(self):
        # The loop sleeps on the stop Event and bails as soon as recording is
        # false — verify a started engine with recording=False finishes fast.
        owner = self._owner(recording=False, frames=[self._pcm_seconds(3.0)])
        engine = self._make_engine(owner, interval_s=0.01)
        events, emit = self._events()
        lock = _CapturingLock()
        with patch.object(self.preview, "transcribe_preview", lambda *_a, **_k: "x"), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine.start()
            thread = engine._thread
            engine.stop()
            if thread is not None:
                thread.join(timeout=2.0)
                self.assertFalse(thread.is_alive(), "preview thread did not exit promptly")
        self.assertEqual(events, [], "no preview while recording is false")

    # ── native-rate capture is resampled in the preview path too ──────────────

    def test_native_rate_preview_input_is_resampled_to_16k(self):
        # When the owner captures at 48k (a Yeti opened after a 16k open was
        # rejected) the preview must resample to 16k before the cheap decode —
        # 3 s of 48k audio (144000 samples) becomes ~48000 samples (3 s @ 16k),
        # NOT the raw 144000. The window is in CAPTURE-rate samples so 3 s < the
        # 15 s window and the whole buffer is decoded.
        owner = self._owner(frames=[self.np.zeros((48000 * 3, 1), dtype=self.np.int16)])
        owner._capture_rate = 48000
        engine = self._make_engine(owner)
        events, emit = self._events()
        lock = _CapturingLock()
        seen = []

        def _fake_transcribe(model, pcm, lang):
            seen.append(len(pcm))
            return "native preview"

        with patch.object(self.preview, "transcribe_preview", _fake_transcribe), \
                patch.object(self.preview, "TRANSCRIBE_LOCK", lock), \
                patch.object(self.preview, "_emit_worker_event", emit):
            engine._tick()

        self.assertEqual(len(seen), 1)
        self.assertTrue(abs(seen[0] - 48000) <= 2,
                        f"expected ~48000 samples at 16k, got {seen[0]}")
        # recording_s reflects real elapsed audio (3 s), computed via capture rate.
        prev = [e for e in events if e.get("state") == "preview"]
        self.assertEqual(len(prev), 1)
        self.assertAlmostEqual(prev[0]["recording_s"], 3.0, places=1)


if __name__ == "__main__":
    unittest.main()
