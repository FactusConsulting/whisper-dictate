"""Unit tests for the Rust-stdin audio source decoder.

The decoder is the Python side of the new ``audio-in-rust`` pipeline:
it parses line-delimited JSON events emitted by the Rust controller's
``audio::PipelineEvent`` serializer. These tests pin down the wire
format so the Rust serializer + Python decoder stay in lock-step.
"""
from __future__ import annotations

import base64
import io
import json

import numpy as np
import pytest

from whisper_dictate.vp_rust_audio_source import (
    FRAME_SAMPLES,
    RustAudioEvent,
    RustStdinProtocolError,
    decode_event,
    iter_events,
)


def _frame_line(samples: np.ndarray) -> str:
    return json.dumps(
        {
            "type": "frame",
            "samples": base64.b64encode(samples.astype("<f4").tobytes()).decode("ascii"),
        }
    )


def test_decode_speech_start_event():
    event = decode_event(json.dumps({"type": "speech_start"}))
    assert event == RustAudioEvent(kind="speech_start")


def test_decode_speech_end_event():
    event = decode_event(json.dumps({"type": "speech_end"}))
    assert event == RustAudioEvent(kind="speech_end")


def test_decode_frame_event_round_trips_samples():
    samples = np.linspace(-1.0, 1.0, FRAME_SAMPLES, dtype="<f4")
    event = decode_event(_frame_line(samples))
    assert event is not None
    assert event.kind == "frame"
    assert event.samples is not None
    assert event.samples.dtype == np.dtype("<f4")
    assert event.samples.shape == (FRAME_SAMPLES,)
    np.testing.assert_allclose(event.samples, samples)


def test_decode_cancelled_event():
    event = decode_event(json.dumps({"type": "cancelled"}))
    assert event == RustAudioEvent(kind="cancelled")


def test_decode_device_error_preserves_message():
    event = decode_event(json.dumps({"type": "device_error", "message": "no device"}))
    assert event is not None
    assert event.kind == "device_error"
    assert event.message == "no device"


def test_decode_device_error_default_message_when_missing():
    event = decode_event(json.dumps({"type": "device_error"}))
    assert event is not None
    assert event.kind == "device_error"
    assert event.message == "unknown error"


def test_blank_lines_yield_none_and_do_not_raise():
    assert decode_event("") is None
    assert decode_event("   \n") is None


def test_invalid_json_raises_protocol_error():
    with pytest.raises(RustStdinProtocolError):
        decode_event("not-json")


def test_unknown_event_type_raises_protocol_error():
    with pytest.raises(RustStdinProtocolError):
        decode_event(json.dumps({"type": "what-is-this"}))


def test_frame_event_missing_samples_raises_protocol_error():
    with pytest.raises(RustStdinProtocolError):
        decode_event(json.dumps({"type": "frame"}))


def test_frame_event_wrong_length_raises_protocol_error():
    short = np.zeros(FRAME_SAMPLES - 1, dtype="<f4")
    with pytest.raises(RustStdinProtocolError):
        decode_event(_frame_line(short))


def test_frame_event_invalid_base64_raises_protocol_error():
    with pytest.raises(RustStdinProtocolError):
        decode_event(json.dumps({"type": "frame", "samples": "$$not-base64$$"}))


def test_frame_event_misaligned_bytes_raises_protocol_error():
    # Decode to 6 bytes — not a multiple of 4 (sizeof f32). Mirrors what
    # would happen if the Rust side ever shipped a truncated frame: the
    # raw numpy.frombuffer error would be opaque, so the decoder wraps
    # it as a ProtocolError including the offending byte count.
    misaligned_b64 = base64.b64encode(b"\x00\x00\x00\x00\x00\x00").decode("ascii")
    with pytest.raises(RustStdinProtocolError) as excinfo:
        decode_event(json.dumps({"type": "frame", "samples": misaligned_b64}))
    msg = str(excinfo.value)
    assert "6" in msg, f"error message must include offending byte count, got {msg!r}"
    assert "multiple of 4" in msg or "f32" in msg, (
        f"error message must describe the alignment requirement, got {msg!r}"
    )


def test_iter_events_walks_a_stream_and_skips_blanks():
    samples = np.zeros(FRAME_SAMPLES, dtype="<f4")
    lines = [
        json.dumps({"type": "speech_start"}),
        "",
        _frame_line(samples),
        json.dumps({"type": "speech_end"}),
    ]
    stream = io.StringIO("\n".join(lines) + "\n")
    events = list(iter_events(stream))
    assert [e.kind for e in events] == ["speech_start", "frame", "speech_end"]


def test_iter_events_protocol_callback_swallows_errors():
    errors: list[RustStdinProtocolError] = []
    stream = io.StringIO("not-json\n" + json.dumps({"type": "speech_end"}) + "\n")
    events = list(iter_events(stream, on_protocol_error=errors.append))
    assert len(errors) == 1
    assert [e.kind for e in events] == ["speech_end"]


def test_iter_events_without_callback_propagates_protocol_error():
    stream = io.StringIO("not-json\n")
    with pytest.raises(RustStdinProtocolError):
        list(iter_events(stream))
