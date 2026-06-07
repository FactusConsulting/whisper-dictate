"""Audio-capture state machine for the live dictation loop (``CaptureMixin``).

Extracted from vp_dictate so the recording I/O — the arecord (PipeWire route)
and sounddevice (direct ALSA) backends, the per-chunk frame accumulation, the
metered audio-level events and the recording-duration helper — lives as a
self-contained mixin, mirroring InjectMixin / KeyBackendMixin. ``Dictate`` mixes
this in and the methods drive the same ``self.`` capture state set in
``Dictate.__init__`` (``self.frames``, ``self.recording``, ``self._stream`` …).

numpy / sounddevice stay lazy: importing this module must not drag in the heavy
audio stack. ``np`` / ``SR`` / ``_find_arecord_device`` are populated by
``_load_runtime_modules()`` (the unit tests patch them on this module), and the
chosen arecord device is owned here so the capture methods and the ``Dictate``
orchestrator agree on a single source of truth.
"""
from __future__ import annotations

import subprocess
import sys  # noqa: F401 — patched by capture tests (sys.modules['sounddevice'])
import threading
import time

from whisper_dictate.vp_events import (
    _audio_level_metrics, _emit_worker_event,
    _sounddevice_capture_channel_candidates, _sounddevice_input_channels,
    _sounddevice_input_name, _sounddevice_stream_kwargs,
)

FIRST_AUDIO_WAIT_S = 0.35

_ARECORD_DEVICE: str | None = None  # set once at startup

# Populated lazily by _load_runtime_modules() (numpy + the arecord probe).
np = None
SR = 16000
_find_arecord_device = None


def _load_runtime_modules() -> None:
    """Populate the lazy numpy + arecord-probe globals used by CaptureMixin.

    Safe to call repeatedly. Kept here so the capture methods resolve ``np`` /
    ``SR`` / ``_find_arecord_device`` from this module's namespace (which is also
    what the capture unit tests patch).
    """
    global np, SR, _find_arecord_device

    import numpy as np  # noqa: F811
    from whisper_dictate.vp_audio import _find_arecord_device  # noqa: F811
    from whisper_dictate.vp_transcribe import SR  # noqa: F811


def _arecord_device() -> str | None:
    """Return the arecord device chosen at startup (None ⇒ use sounddevice)."""
    return _ARECORD_DEVICE


def _ensure_arecord_device() -> str | None:
    """Probe for the PipeWire arecord route, caching a found device.

    A truthy device string is cached after the first successful probe. ``None``
    doubles as the "not probed yet" sentinel, so while no device is found this
    re-probes on each call (returning None ⇒ direct ALSA via sounddevice).
    Mirrors the discovery that used to run inline in ``Dictate.__init__``.
    """
    global _ARECORD_DEVICE
    if _ARECORD_DEVICE is None:
        _ARECORD_DEVICE = _find_arecord_device()
    return _ARECORD_DEVICE


class CaptureMixin:
    def _cb(self, indata, frames, t, status):
        if self.recording:
            if not self._first_audio_event.is_set():
                self._first_audio_at = time.monotonic()
                self._record_started = self._first_audio_at
                self._first_audio_event.set()
            chunk = indata.copy()
            self.frames.append(chunk)
            self._emit_audio_level(chunk)

    def _arecord_reader(self, proc):
        # Read raw S16_LE mono 16kHz from arecord stdout into self.frames
        chunk = SR * 2 * 1  # 1 second of S16 mono = SR*2 bytes
        while self.recording:
            data = proc.stdout.read(chunk // 8)  # read ~125ms chunks
            if not data:
                break
            arr = np.frombuffer(data, dtype=np.int16).reshape(-1, 1)
            if not self._first_audio_event.is_set():
                self._first_audio_at = time.monotonic()
                self._record_started = self._first_audio_at
                self._first_audio_event.set()
            self.frames.append(arr)
            self._emit_audio_level(arr)

    def _emit_audio_level(self, pcm) -> None:
        now = time.monotonic()
        if now - self._last_audio_level_event < 0.12:
            return
        raw_dbfs, peak, level = _audio_level_metrics(pcm)
        self._last_audio_level_event = now
        _emit_worker_event(
            "audio",
            state="recording",
            level=round(level, 3),
            raw_dbfs=round(raw_dbfs, 1),
            peak=round(peak, 3),
            capture_backend=self._capture_backend,
            audio_device=self._audio_input_device,
            capture_channels=self._capture_channels,
        )

    def _start_arecord(self) -> tuple[str, str]:
        self._capture_backend = "arecord"
        self._audio_input_device = f"arecord -D {_ARECORD_DEVICE}"
        self._capture_channels = 1
        self._arecord_proc = subprocess.Popen(
            ["arecord", "-D", _ARECORD_DEVICE, "-f", "S16_LE",
             "-r", str(SR), "-c", "1", "-"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        threading.Thread(
            target=self._arecord_reader,
            args=(self._arecord_proc,),
            daemon=True,
        ).start()
        return self._capture_backend, self._audio_input_device

    def _start_sounddevice(self) -> tuple[str, str]:
        import sounddevice as sd
        self._capture_backend = "sounddevice"
        self._audio_input_device = _sounddevice_input_name(sd) or "sounddevice default input"
        last_error = None
        for channels in _sounddevice_capture_channel_candidates(_sounddevice_input_channels(sd)):
            self._capture_channels = channels
            for kwargs in _sounddevice_stream_kwargs(self._capture_channels, self._cb):
                try:
                    self._stream = sd.InputStream(**kwargs)
                    break
                except Exception as exc:
                    last_error = exc
                    self._stream = None
            if self._stream is not None:
                break
        if self._stream is None:
            raise last_error
        self._stream.start()
        return self._capture_backend, self._audio_input_device

    def _stop_capture_streams(self) -> None:
        if self._arecord_proc:
            self._arecord_proc.terminate()
            self._arecord_proc.wait()
            self._arecord_proc = None
        if self._stream:
            self._stream.stop()
            self._stream.close()
            self._stream = None

    def _recording_seconds(self, pcm) -> float:
        if self._record_started:
            return time.monotonic() - self._record_started
        return len(pcm) / SR
