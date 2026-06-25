"""Integration test for the Rust-stdin capture path (CaptureMixin shim).

Drives :func:`vp_capture_rust_stdin.start_rust_stdin_capture` against a
fake :class:`CaptureMixin` and an in-memory stream of JSON lines that
mimic what the Rust controller would write. Verifies:

* ``frame`` events are decoded and appended to ``mixin.frames`` as
  int16-shaped arrays the existing transcription pipeline expects.
* The first frame marks ``_first_audio_event`` so the recording-start UI
  fires identically to the sounddevice path.
* ``device_error`` exits the reader without re-raising — the supervisor
  receives the same error on its own channel and tears down.
* A corrupt line is skipped (logged) rather than killing the reader; a
  later valid frame still lands in the buffer.

The pure decoder (:mod:`vp_rust_audio_source`) already has unit tests;
this file pins the GLUE between decoder and CaptureMixin.
"""
from __future__ import annotations

import base64
import io
import json
import threading
import time
import unittest

import numpy as np

from whisper_dictate.vp_capture_rust_stdin import start_rust_stdin_capture
from whisper_dictate.vp_rust_audio_source import FRAME_SAMPLES


def _frame_line(samples: np.ndarray) -> str:
    return json.dumps({
        "type": "frame",
        "samples": base64.b64encode(samples.astype("<f4").tobytes()).decode("ascii"),
    })


class _FakeMixin:
    """Minimal stand-in for `CaptureMixin` that captures what the reader does.

    The reader only touches a small surface: ``frames``, ``recording``,
    ``_first_audio_event``, ``_first_audio_at``, ``_record_started``,
    ``_emit_audio_level``, plus a handful of capture-backend attribute
    setters. Mirroring just those keeps the test independent of the
    1k-line :class:`vp_capture.CaptureMixin` (which would otherwise drag
    in sounddevice + pynput + numpy stubs).
    """

    def __init__(self):
        self.frames: list[np.ndarray] = []
        self.recording = True
        self._first_audio_event = threading.Event()
        self._first_audio_at = 0.0
        self._record_started = 0.0
        self._capture_backend = ""
        self._audio_input_device = ""
        self._capture_channels = 0
        self._capture_rate = 0
        self._capture_dtype = ""
        self.audio_level_calls: list[np.ndarray] = []

    def _emit_audio_level(self, chunk):
        self.audio_level_calls.append(chunk)


def _wait_for(predicate, *, timeout: float = 2.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return False


class RustStdinCaptureGlueTests(unittest.TestCase):
    def test_frame_events_are_decoded_and_appended_as_int16(self):
        # Three frames: silence, half-amplitude, full-amplitude. The
        # int16 conversion must clip on the way through (1.0 → 32767).
        frames = [
            np.zeros(FRAME_SAMPLES, dtype="<f4"),
            np.full(FRAME_SAMPLES, 0.5, dtype="<f4"),
            np.full(FRAME_SAMPLES, 1.0, dtype="<f4"),
        ]
        lines = (
            json.dumps({"type": "speech_start"}) + "\n"
            + "\n".join(_frame_line(f) for f in frames) + "\n"
            + json.dumps({"type": "speech_end"}) + "\n"
        )
        stream = io.StringIO(lines)
        mixin = _FakeMixin()

        backend, device = start_rust_stdin_capture(mixin, stream=stream)
        self.assertEqual(backend, "rust-stdin")
        self.assertEqual(device, "rust-stdin")
        self.assertEqual(mixin._capture_channels, 1)
        self.assertEqual(mixin._capture_rate, 16_000)
        self.assertEqual(mixin._capture_dtype, "int16")

        # Reader runs on a daemon thread; wait until all three frames land.
        self.assertTrue(
            _wait_for(lambda: len(mixin.frames) >= 3),
            f"expected 3 frames, got {len(mixin.frames)}",
        )
        # The reader exits on its own once the stream EOFs; join the
        # thread to make the rest of the test deterministic.
        mixin._rust_stdin_thread.join(timeout=2.0)
        self.assertFalse(mixin._rust_stdin_thread.is_alive())

        self.assertEqual(len(mixin.frames), 3)
        for f in mixin.frames:
            self.assertEqual(f.dtype, np.int16)
            self.assertEqual(f.shape, (FRAME_SAMPLES, 1))
        # Clipping at +1.0 must land exactly at int16 max, not wrap.
        self.assertEqual(int(mixin.frames[2].max()), 32767)
        # First frame must have triggered the first-audio bookkeeping.
        self.assertTrue(mixin._first_audio_event.is_set())
        self.assertGreater(mixin._first_audio_at, 0.0)

    def test_device_error_terminates_reader_without_raising(self):
        lines = (
            _frame_line(np.zeros(FRAME_SAMPLES, dtype="<f4")) + "\n"
            + json.dumps({"type": "device_error", "message": "mic unplugged"}) + "\n"
            # This frame must NOT land — the reader exits on device_error.
            + _frame_line(np.full(FRAME_SAMPLES, 0.5, dtype="<f4")) + "\n"
        )
        mixin = _FakeMixin()
        start_rust_stdin_capture(mixin, stream=io.StringIO(lines))
        mixin._rust_stdin_thread.join(timeout=2.0)
        self.assertFalse(mixin._rust_stdin_thread.is_alive())
        # Exactly one frame appended (the one before device_error).
        self.assertEqual(len(mixin.frames), 1)

    def test_corrupt_line_is_skipped_and_next_frame_still_lands(self):
        lines = (
            "not-json\n"
            + _frame_line(np.zeros(FRAME_SAMPLES, dtype="<f4")) + "\n"
        )
        mixin = _FakeMixin()
        start_rust_stdin_capture(mixin, stream=io.StringIO(lines))
        self.assertTrue(_wait_for(lambda: len(mixin.frames) >= 1))
        mixin._rust_stdin_thread.join(timeout=2.0)
        # The valid frame after the corrupt line landed.
        self.assertEqual(len(mixin.frames), 1)

    def test_reader_drops_frames_while_recording_false_then_appends_again(self):
        """Iteration-2 finding #2 + #4: the reader is long-lived and
        drops frames when ``recording`` is False, so:

        * No abandoned blocked thread between presses (the previous
          design spawned a fresh reader per press and tried to
          join+abandon it on release).
        * No stale audio from inter-press silence leaks into the next
          press's ``mixin.frames``.

        Drive the reader through three phases on a single gated
        stream: record one frame, release PTT, idle frame must NOT
        land, re-press, second recording frame MUST land — all served
        by the SAME thread.
        """

        class _GatedStream:
            """Yields lines one at a time, each gated on a per-line event."""

            def __init__(self, items):
                # items: list[(line, threading.Event)] — when the gate
                # fires we yield the line. A `None` line ends the stream.
                self._items = list(items)

            def __iter__(self):
                return self

            def __next__(self):
                if not self._items:
                    raise StopIteration
                line, gate = self._items.pop(0)
                gate.wait(timeout=2.0)
                if line is None:
                    raise StopIteration
                return line

        first_press = _frame_line(np.zeros(FRAME_SAMPLES, dtype="<f4")) + "\n"
        idle_frame = _frame_line(np.full(FRAME_SAMPLES, 0.25, dtype="<f4")) + "\n"
        second_press = _frame_line(np.full(FRAME_SAMPLES, 0.5, dtype="<f4")) + "\n"
        g1, g2, g3, eof = (
            threading.Event(),
            threading.Event(),
            threading.Event(),
            threading.Event(),
        )

        mixin = _FakeMixin()
        # mixin.recording starts True (set by _FakeMixin.__init__) =
        # simulating "PTT pressed". Start the reader.
        start_rust_stdin_capture(
            mixin,
            stream=_GatedStream(
                [
                    (first_press, g1),
                    (idle_frame, g2),
                    (second_press, g3),
                    (None, eof),
                ]
            ),
        )
        reader = mixin._rust_stdin_thread
        self.assertTrue(reader.is_alive())

        # Press 1: gate the first frame and verify it lands.
        g1.set()
        self.assertTrue(_wait_for(lambda: len(mixin.frames) >= 1))

        # PTT released → recording False. Gate the idle frame; the
        # reader must stay alive (long-lived contract) but the frame
        # must NOT be appended.
        mixin.recording = False
        g2.set()
        # Give the reader a beat to process the dropped frame; we can't
        # easily observe "did nothing" so settle on a small sleep.
        time.sleep(0.1)
        self.assertEqual(
            len(mixin.frames), 1,
            "idle frame must be dropped, not appended",
        )
        self.assertTrue(
            reader.is_alive(),
            "iteration-2 finding #2: reader must survive recording=False",
        )

        # Press 2: re-arm recording, release the second frame, verify
        # it lands — proves the SAME thread serves both presses.
        mixin.recording = True
        g3.set()
        self.assertTrue(_wait_for(lambda: len(mixin.frames) >= 2))
        # And no second thread was spawned (the cached one is reused).
        self.assertIs(mixin._rust_stdin_thread, reader)

        # Tear down: close the stream and verify the reader exits on
        # its own.
        eof.set()
        reader.join(timeout=2.0)
        self.assertFalse(reader.is_alive())

    def test_second_start_call_reuses_existing_reader_thread(self):
        """Iteration-2 finding #2: ``start_rust_stdin_capture`` is
        idempotent — calling it a second time (next PTT press) must
        NOT spawn a second reader thread. Two readers competing for
        ``for line in sys.stdin`` would interleave bytes between
        themselves and break the wire contract.
        """
        # An endlessly-blocking stream so the first reader stays parked
        # on `next(stream)` for the duration of the test.
        class _BlockingStream:
            def __iter__(self):
                return self

            def __next__(self):
                threading.Event().wait()  # blocks forever

        mixin = _FakeMixin()
        start_rust_stdin_capture(mixin, stream=_BlockingStream())
        first_thread = mixin._rust_stdin_thread
        self.assertTrue(first_thread.is_alive())
        # Second call (simulates the next PTT press) — must reuse.
        start_rust_stdin_capture(mixin, stream=_BlockingStream())
        self.assertIs(
            mixin._rust_stdin_thread, first_thread,
            "second start must reuse the already-running reader",
        )


if __name__ == "__main__":
    unittest.main()
