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
        enabled = self.preview.preview_enabled
        # Local whisper backend with a positive interval → enabled.
        self.assertTrue(enabled(3.0, "whisper"))
        # Disabled when interval is 0 (or negative).
        self.assertFalse(enabled(0.0, "whisper"))
        self.assertFalse(enabled(-1.0, "whisper"))
        # Cloud (paid API) and Parakeet are never previewed.
        self.assertFalse(enabled(3.0, "openai"))
        self.assertFalse(enabled(3.0, "parakeet"))
        # Case / whitespace tolerant.
        self.assertTrue(enabled(3.0, "  Whisper "))

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


if __name__ == "__main__":
    unittest.main()
