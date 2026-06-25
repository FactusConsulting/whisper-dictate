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

    def test_recording_flag_flip_exits_reader_cleanly(self):
        # Frame, then we set recording=False BEFORE EOF. The reader
        # checks `recording` per event and must exit even though the
        # stream has more bytes left.
        first = _frame_line(np.zeros(FRAME_SAMPLES, dtype="<f4")) + "\n"
        # A controllable stream that blocks on the second read until
        # we've flipped `recording`.
        class _GatedStream:
            def __init__(self, head: str, gate: threading.Event):
                self._chunks = [head]
                self._gate = gate

            def __iter__(self):
                return self

            def __next__(self):
                if self._chunks:
                    return self._chunks.pop(0)
                # Wait for the test to release us with a final EOF.
                self._gate.wait(timeout=2.0)
                raise StopIteration

        gate = threading.Event()
        mixin = _FakeMixin()
        start_rust_stdin_capture(mixin, stream=_GatedStream(first, gate))
        self.assertTrue(_wait_for(lambda: len(mixin.frames) >= 1))
        # Flip the recording flag (the supervisor / PTT release does this)
        # then release the gate so the iterator returns StopIteration.
        mixin.recording = False
        gate.set()
        mixin._rust_stdin_thread.join(timeout=2.0)
        self.assertFalse(mixin._rust_stdin_thread.is_alive())


if __name__ == "__main__":
    unittest.main()
