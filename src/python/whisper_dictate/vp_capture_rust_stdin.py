"""Glue between :mod:`vp_rust_audio_source` and :class:`vp_capture.CaptureMixin`.

When :class:`whisper_dictate.vp_dictate.Dictate` is constructed with
``audio_source="rust-stdin"`` (set by the runtime when the user opted into
``VOICEPI_AUDIO_BACKEND=rust`` AND the Rust binary was built with the
``audio-in-rust`` cargo feature), every PTT press skips the normal
sounddevice / arecord open and instead reads frames from stdin.

The Rust controller (`src/rust/audio/stdin_bridge.rs`) writes one JSON
event per line into the worker process' stdin. We spawn a SINGLE
long-lived daemon thread (per worker process) that decodes those events
via :func:`vp_rust_audio_source.iter_events` and folds the audio into
the existing :class:`CaptureMixin` frame buffer — the rest of the
pipeline (transcription, injection, post-processing) is unchanged.

Phase-1 scope: forward ``frame`` events into ``self.frames`` and treat
``speech_end`` / ``cancelled`` / ``device_error`` as terminal hints (the
PTT release decides when to flush; the events here are mostly
informational for the supervisor). A later phase will let the Rust VAD
drive utterance boundaries directly.

**Threading model (iteration-2 review finding #2):** the reader thread
is started ONCE per worker process — not once per PTT press — and runs
continuously until stdin EOFs (the supervisor closes the Rust bridge
at worker shutdown). When ``mixin.recording`` is False, the reader
DROPS incoming frames instead of buffering them; this serves two
purposes:

* No abandoned blocked threads: the previous design spawned a fresh
  thread per press and then "best-effort joined with timeout" it on
  PTT release. If the thread was blocked in ``for line in sys.stdin``
  during silence, the timeout expired, the handle got cleared, and the
  next press spawned a SECOND reader on the same stdin — both racing
  to consume the next frame. With one long-lived reader, no thread is
  ever abandoned and there is no "stale reader stealing the next
  press's audio" race.
* No stale audio between presses: because the single reader keeps
  draining stdin even when ``recording`` is False, the OS pipe buffer
  never fills with idle-time audio that would otherwise leak into the
  next PTT press's ``mixin.frames``.

The complementary Rust-side change — gating cpal/VAD on PTT so the
pipeline doesn't *produce* audio when no one is recording — is
deferred to a follow-up PR because it needs a Python→Rust control
channel that doesn't exist yet (see the PR description). The
Python-side drop-when-idle here is a self-contained mitigation that
prevents the user-visible regression (stale audio in next utterance,
pipe-full deadlocks) without that control plane.
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
    """Ensure the long-lived Rust-stdin reader is running on `mixin`.

    Idempotent: the first call spawns the reader; subsequent calls
    (one per PTT press) just confirm it's alive and return the
    backend/device labels. The reader runs until stdin EOFs.

    Returns the ``(capture_backend, audio_input_device)`` tuple used by
    :func:`_emit_audio_level` and the recording-status worker events. The
    Rust pipeline is the source of truth for the device name, but we
    haven't yet plumbed that through the JSON protocol — for now we
    label the device as ``"rust-stdin"`` so the runtime log clearly
    distinguishes which capture path ran.

    The reader thread:

    * Marks ``mixin._first_audio_event`` on the first ``frame`` event
      OF A RECORDING (not the first frame ever, since the reader is
      long-lived and frames may arrive before the first press).
    * Converts the f32 frame to the int16 that ``mixin.frames`` expects.
    * Emits the throttled audio-level event so the live audio meter
      keeps animating.
    * DROPS frames silently when ``mixin.recording`` is False (between
      PTT presses), so the pipe is kept drained but no idle audio
      reaches the transcriber.
    * Exits when the stream closes (Rust controller hung up / supervisor
      stopped the worker), NOT when ``mixin.recording`` flips back to
      False — the same reader serves every press of this worker's life.
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

    # Iteration-2 review finding #2: if a reader thread is already
    # alive (started by a previous press), reuse it. We MUST NOT spawn
    # a second reader on the same stdin — two threads competing for
    # ``for line in sys.stdin`` would interleave frames and leak audio
    # between presses. The previous design tried to join+clear the
    # thread on PTT release; if the thread was blocked in stdin.readline
    # the join timed out and the next press happily started a stale
    # second reader.
    existing = getattr(mixin, "_rust_stdin_thread", None)
    if existing is not None and existing.is_alive():
        return mixin._capture_backend, mixin._audio_input_device

    def _reader() -> None:
        last_level_t = 0.0
        try:
            for event in iter_events(
                stream, on_protocol_error=_log_protocol_error
            ):
                handled = _handle_event(mixin, event, np)
                if handled is False:
                    # Terminal event (device_error). Surface it and
                    # leave the loop; the supervisor sees the same
                    # device_error on its error channel and will tear
                    # down the worker.
                    return
                # Throttle live audio-level events the same way the
                # sounddevice callback does (120 ms = ~8 Hz UI updates).
                # Only emit while actually recording — idle frames are
                # dropped (see _handle_event) and shouldn't update the
                # meter either.
                if (
                    event.kind == "frame"
                    and getattr(mixin, "recording", False)
                    and mixin.frames
                ):
                    now = time.monotonic()
                    if now - last_level_t >= 0.12:
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
    """Apply one decoded event to the mixin. Returns False on terminal.

    Frames are DROPPED when ``mixin.recording`` is False — the reader
    keeps draining stdin (so the pipe never blocks the Rust writer)
    but no idle-time audio reaches ``mixin.frames`` between PTT
    presses.
    """
    if event.kind == "frame":
        if event.samples is None or event.samples.size != FRAME_SAMPLES:
            return True  # protocol error already raised by decoder
        # Iteration-2 review finding #4 (partial): drop frames when no
        # PTT recording is active. The full fix also gates cpal/VAD on
        # the Rust side (requires a Python→Rust control channel),
        # tracked as a follow-up; this Python-side drop alone already
        # prevents stale audio from leaking into the next press AND
        # keeps the OS pipe buffer drained so the Rust writer never
        # blocks on a full pipe.
        if not getattr(mixin, "recording", False):
            return True
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
