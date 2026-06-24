"""Rust-stdin audio source: line-delimited JSON frames from the Rust pipeline.

When the supervisor launches the worker with ``--audio-source=rust-stdin``,
the Rust controller pipes a stream of JSON messages into the worker's
stdin instead of opening a sounddevice/arecord stream of its own.

Message vocabulary (one JSON object per stdin line):

``{"type": "frame", "samples": "<base64>"}``
    A 480-sample 30 ms frame at 16 kHz, encoded as little-endian f32
    (``numpy.float32``) → base64. Decode with
    ``numpy.frombuffer(base64.b64decode(s), dtype='<f4')``. Sequencing
    matches the Rust ``PipelineEvent::Frame`` order; consumers accumulate
    them between ``speech_start`` and ``speech_end``.

``{"type": "speech_start"}``
    The Rust-side VAD committed to a new utterance (onset debounce
    crossed). Consumer should reset the utterance buffer and prepare for
    a burst of ``frame`` messages.

``{"type": "speech_end"}``
    The Rust-side VAD ended the utterance (hangover expired). Consumer
    should flush the utterance buffer to the transcriber.

``{"type": "device_error", "message": "..."}``
    The Rust pipeline failed unrecoverably. Consumer should surface this
    to the user and tear down — no further messages will arrive.

This module deliberately holds NO references to numpy at import time:
the runtime worker imports numpy lazily everywhere else (see vp_capture)
so ``--help`` and ``--doctor`` stay snappy on systems where the ML stack
isn't installed. ``RustStdinAudioSource.read_frame`` does the lazy import.

**Status:** the reader + message vocabulary land in this PR. The wiring
that REPLACES :class:`whisper_dictate.vp_capture.CaptureMixin`'s
sounddevice/arecord open call with a ``RustStdinAudioSource`` is TODO —
see the PR description and the ``audio-in-rust`` feature note in
``src/rust/Cargo.toml``. Until then this module is exercised only by its
own unit tests; the default audio path is unchanged.
"""
from __future__ import annotations

import base64
import json
import sys
from dataclasses import dataclass
from typing import IO, Any, Callable, Iterator, Optional

FRAME_SAMPLES = 480
"""Expected length of a ``frame`` payload after decoding. Mirrors the
Rust ``audio::resampler::FRAME_SIZE``."""

SAMPLE_RATE = 16_000
"""Sample rate of decoded frames in Hz. Mirrors the Rust
``audio::resampler::OUTPUT_RATE``."""


@dataclass(frozen=True)
class RustAudioEvent:
    """Decoded message from the Rust pipeline.

    ``kind`` is one of ``"frame"``, ``"speech_start"``, ``"speech_end"``
    or ``"device_error"``. ``samples`` is populated only for ``frame``
    events (as a ``numpy.ndarray`` of dtype ``float32`` and length
    :data:`FRAME_SAMPLES`). ``message`` is populated only for
    ``device_error``.
    """

    kind: str
    samples: Optional[Any] = None
    message: Optional[str] = None


class RustStdinProtocolError(RuntimeError):
    """Raised when an incoming line cannot be parsed as a valid event."""


def decode_event(line: str) -> Optional[RustAudioEvent]:
    """Parse one JSON line into a :class:`RustAudioEvent`.

    Returns ``None`` for blank lines (so a producer that emits a trailing
    newline or stray whitespace doesn't kill the reader). Raises
    :exc:`RustStdinProtocolError` on malformed JSON or unknown event
    types — the caller decides whether to log+continue or tear down.
    """
    raw = line.strip()
    if not raw:
        return None
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise RustStdinProtocolError(f"invalid JSON from rust audio: {exc}") from exc
    if not isinstance(payload, dict):
        raise RustStdinProtocolError(
            f"expected JSON object, got {type(payload).__name__}"
        )
    kind = payload.get("type")
    if kind == "frame":
        samples_b64 = payload.get("samples")
        if not isinstance(samples_b64, str):
            raise RustStdinProtocolError("frame event missing base64 'samples' string")
        # numpy is imported lazily so the worker's --help / --doctor stay
        # fast on machines without the ML stack installed (mirrors the
        # rest of the runtime). We do the import inside the function so
        # the module's import cost is zero.
        import numpy as np
        try:
            raw_bytes = base64.b64decode(samples_b64, validate=True)
        except (ValueError, base64.binascii.Error) as exc:  # type: ignore[attr-defined]
            raise RustStdinProtocolError(f"frame samples base64 invalid: {exc}") from exc
        samples = np.frombuffer(raw_bytes, dtype="<f4")
        if samples.size != FRAME_SAMPLES:
            raise RustStdinProtocolError(
                f"frame samples wrong length: got {samples.size}, want {FRAME_SAMPLES}"
            )
        return RustAudioEvent(kind="frame", samples=samples)
    if kind == "speech_start":
        return RustAudioEvent(kind="speech_start")
    if kind == "speech_end":
        return RustAudioEvent(kind="speech_end")
    if kind == "device_error":
        message = str(payload.get("message") or "").strip()
        return RustAudioEvent(kind="device_error", message=message or "unknown error")
    raise RustStdinProtocolError(f"unknown rust audio event type: {kind!r}")


def iter_events(
    stream: Optional[IO[str]] = None,
    *,
    on_protocol_error: Optional[Callable[[RustStdinProtocolError], None]] = None,
) -> Iterator[RustAudioEvent]:
    """Yield decoded events from a stream of JSON lines.

    ``stream`` defaults to ``sys.stdin``. Stops at EOF (which the Rust
    side hits when the supervisor closes the pipe at shutdown). Protocol
    errors are forwarded to ``on_protocol_error`` and the offending line
    is skipped, so a single corrupt frame doesn't end the recording —
    unless the caller raises in the callback.
    """
    if stream is None:
        stream = sys.stdin
    for line in stream:
        try:
            event = decode_event(line)
        except RustStdinProtocolError as exc:
            if on_protocol_error is not None:
                on_protocol_error(exc)
                continue
            raise
        if event is None:
            continue
        yield event


__all__ = [
    "FRAME_SAMPLES",
    "SAMPLE_RATE",
    "RustAudioEvent",
    "RustStdinProtocolError",
    "decode_event",
    "iter_events",
]
