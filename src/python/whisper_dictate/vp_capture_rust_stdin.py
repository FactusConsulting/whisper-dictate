"""Glue between :mod:`vp_rust_audio_source` and :class:`vp_capture.CaptureMixin`.

When :class:`whisper_dictate.vp_dictate.Dictate` is constructed with
``audio_source="rust-stdin"`` (set by the runtime when the user opted into
``VOICEPI_AUDIO_BACKEND=rust`` AND the Rust binary was built with the
``audio-in-rust`` cargo feature), every PTT press skips the normal
sounddevice / arecord open and instead reads frames from stdin.

The Rust controller (`src/rust/audio/stdin_bridge.rs`) writes one JSON
event per line into the worker process' stdin. We spawn a daemon thread
that decodes those events via :func:`vp_rust_audio_source.iter_events`
and folds the audio into the existing :class:`CaptureMixin` frame buffer
— the rest of the pipeline (transcription, injection, post-processing)
is unchanged.

Phase-1 scope: forward ``frame`` events into ``self.frames`` and treat
``speech_end`` / ``cancelled`` / ``device_error`` as terminal hints (the
PTT release decides when to flush; the events here are mostly
informational for the supervisor). A later phase will let the Rust VAD
drive utterance boundaries directly.
"""
from __future__ import annotations

import sys
import threading
import time
from typing import IO, Optional

from .vp_rust_audio_source import (
    FRAME_SAMPLES,
    SAMPLE_RATE,
    RustAudioEvent,
    RustStdinProtocolError,
    iter_events,
)


def start_rust_stdin_capture(
    mixin,
    *,
    stream: Optional[IO[str]] = None,
) -> tuple[str, str]:
    """Spawn the Rust-stdin reader thread on `mixin` (a `CaptureMixin`).

    Returns the ``(capture_backend, audio_input_device)`` tuple used by
    :func:`_emit_audio_level` and the recording-status worker events. The
    Rust pipeline is the source of truth for the device name, but we
    haven't yet plumbed that through the JSON protocol — for now we
    label the device as ``"rust-stdin"`` so the runtime log clearly
    distinguishes which capture path ran.

    The reader thread:

    * Marks ``mixin._first_audio_event`` on the first ``frame`` event
      so the existing recording-start UI fires identically to the
      sounddevice path.
    * Converts the f32 frame to the int16 that ``mixin.frames`` expects.
    * Emits the throttled audio-level event so the live audio meter
      keeps animating.
    * Exits when the stream closes (Rust controller hung up / supervisor
      stopped us) OR when ``mixin.recording`` flips back to False
      (normal PTT release).
    """
    # Imported lazily to avoid pulling numpy at module-import time —
    # mirrors the rest of vp_capture's "no numpy until we record" rule.
    import numpy as np

    if stream is None:
        stream = sys.stdin

    mixin._capture_backend = "rust-stdin"
    mixin._audio_input_device = "rust-stdin"
    mixin._capture_channels = 1
    mixin._capture_rate = SAMPLE_RATE
    mixin._capture_dtype = "int16"

    def _reader() -> None:
        last_level_t = 0.0
        try:
            for event in iter_events(
                stream, on_protocol_error=_log_protocol_error
            ):
                if not getattr(mixin, "recording", False):
                    # PTT released between events; the supervisor will
                    # stop the bridge shortly. Exit cleanly so the next
                    # press starts a fresh reader.
                    return
                handled = _handle_event(mixin, event, np)
                if handled is False:
                    # Terminal event (device_error). Surface it and
                    # leave the loop; the supervisor sees the same
                    # device_error on its error channel and will tear
                    # down the worker.
                    return
                # Throttle live audio-level events the same way the
                # sounddevice callback does (120 ms = ~8 Hz UI updates).
                now = time.monotonic()
                if event.kind == "frame" and now - last_level_t >= 0.12:
                    last_level_t = now
                    mixin._emit_audio_level(_last_chunk(mixin))
        except Exception as exc:  # noqa: BLE001 — reader thread must never escape
            print(f"[cap] rust-stdin reader error: {exc}",
                  file=sys.stderr, flush=True)

    thread = threading.Thread(target=_reader, name="rust-stdin-reader", daemon=True)
    mixin._rust_stdin_thread = thread
    thread.start()
    return mixin._capture_backend, mixin._audio_input_device


def _handle_event(mixin, event: RustAudioEvent, np) -> bool:
    """Apply one decoded event to the mixin. Returns False on terminal."""
    if event.kind == "frame":
        if event.samples is None or event.samples.size != FRAME_SAMPLES:
            return True  # protocol error already raised by decoder
        if not mixin._first_audio_event.is_set():
            mixin._first_audio_at = time.monotonic()
            mixin._record_started = mixin._first_audio_at
            mixin._first_audio_event.set()
        # f32 in [-1, 1] → int16. clip first so loud frames don't wrap.
        int16 = np.clip(event.samples * 32768.0, -32768.0, 32767.0).astype(np.int16)
        # Existing frame buffer expects shape (n, channels).
        mixin.frames.append(int16.reshape(-1, 1))
        return True
    if event.kind in ("speech_start", "speech_end", "cancelled"):
        # Phase 1: we honour the PTT-release boundary, not the VAD's.
        # The events are useful diagnostics — log at trace level for
        # later iterations where the Rust VAD drives utterance commits.
        return True
    if event.kind == "device_error":
        print(f"[cap] rust-stdin device error: {event.message}",
              file=sys.stderr, flush=True)
        return False
    return True


def _last_chunk(mixin):
    """Return the most recent appended chunk for audio-level metering."""
    if not mixin.frames:
        # Shouldn't happen — we only call this after a frame event.
        # Return a tiny zero array so the metering math doesn't crash.
        import numpy as np
        return np.zeros((1, 1), dtype=np.int16)
    return mixin.frames[-1]


def _log_protocol_error(exc: RustStdinProtocolError) -> None:
    """Forward decoder errors to stderr without killing the reader.

    A single corrupt frame from the Rust controller mustn't end the
    recording — the iterator already skips the bad line, we just leave
    a breadcrumb so the supervisor can investigate.
    """
    print(f"[cap] rust-stdin protocol error (skipped): {exc}",
          file=sys.stderr, flush=True)


__all__ = ["start_rust_stdin_capture"]
