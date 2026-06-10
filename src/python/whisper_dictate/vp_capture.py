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

import os
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


def _audio_device_setting() -> str:
    """The requested input device, read fresh from the env at stream-open time.

    Empty/unset ⇒ system default. Read live (not at import) so a live-reloaded
    VOICEPI_AUDIO_DEVICE takes effect on the next recording without a restart.
    """
    return (os.environ.get("VOICEPI_AUDIO_DEVICE") or "").strip()


def _input_devices(sd) -> list[dict]:
    """Return sounddevice input devices (max_input_channels > 0), index-tagged.

    Pure-ish helper shared by device resolution and the ``--list-audio-devices``
    CLI. Each entry carries its query_devices index so callers can match by
    index or by (substring of) name. Devices without input channels are skipped.
    """
    try:
        devices = sd.query_devices()
    except Exception:
        return []
    result = []
    for index, info in enumerate(devices):
        if not isinstance(info, dict):
            continue
        try:
            channels = int(info.get("max_input_channels") or 0)
        except (TypeError, ValueError):
            channels = 0
        if channels <= 0:
            continue
        result.append({
            "index": index,
            "name": str(info.get("name") or "").strip(),
            "max_input_channels": channels,
        })
    return result


def _resolve_sounddevice_device(sd, value: str):
    """Resolve a VOICEPI_AUDIO_DEVICE value to a sounddevice ``device=`` arg.

    Value semantics:
      * empty/unset       ⇒ ``None`` (sounddevice picks the system default)
      * an integer string ⇒ that device index (int)
      * otherwise         ⇒ case-insensitive substring match against input
                            device names (first match wins); the matched index
                            is returned. No match ⇒ warn + ``None`` (default).
    """
    value = (value or "").strip()
    if not value:
        return None
    if value.lstrip("+-").isdigit():
        return int(value)
    needle = value.casefold()
    for device in _input_devices(sd):
        if needle in device["name"].casefold():
            return device["index"]
    print(
        f"[cap] audio device {value!r} not found, using default",
        file=sys.stderr,
        flush=True,
    )
    return None


def _selected_device_name(sd, device) -> str | None:
    """Human label for a resolved sounddevice ``device=`` arg (or ``None``).

    Used so the status/meter shows the explicitly chosen device's name rather
    than the system default. Returns ``None`` when no specific device was chosen
    or the name can't be resolved, leaving the default-name fallback in place.
    """
    if not isinstance(device, int):
        return None
    for entry in _input_devices(sd):
        if entry["index"] == device:
            return entry["name"] or None
    return None


def _arecord_device_arg(default_device: str | None, value: str) -> str | None:
    """Pick the ALSA/PipeWire device string for the arecord backend.

    A set VOICEPI_AUDIO_DEVICE value is treated as a raw ALSA/PipeWire device
    string and used verbatim (``arecord -D <value>``); otherwise the probed
    default route is kept.
    """
    value = (value or "").strip()
    return value or default_device


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
        custom_device = bool((_audio_device_setting() or "").strip())
        device = _arecord_device_arg(_ARECORD_DEVICE, _audio_device_setting())
        self._audio_input_device = f"arecord -D {device}"
        self._capture_channels = 1
        # Suppress arecord's chatter only for the probed default device. A
        # user-configured -D value can be invalid, and silencing stderr would
        # make that failure undiagnosable (no frames, no error anywhere) — let
        # it flow to the worker's stderr so it lands in the runtime log.
        self._arecord_proc = subprocess.Popen(
            ["arecord", "-D", device, "-f", "S16_LE",
             "-r", str(SR), "-c", "1", "-"],
            stdout=subprocess.PIPE,
            stderr=None if custom_device else subprocess.DEVNULL,
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
        device = _resolve_sounddevice_device(sd, _audio_device_setting())
        self._audio_input_device = (
            _selected_device_name(sd, device)
            or _sounddevice_input_name(sd)
            or "sounddevice default input"
        )
        last_error = None
        for channels in _sounddevice_capture_channel_candidates(_sounddevice_input_channels(sd)):
            self._capture_channels = channels
            for kwargs in _sounddevice_stream_kwargs(self._capture_channels, self._cb):
                if device is not None:
                    kwargs["device"] = device
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
