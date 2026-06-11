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

from whisper_dictate.vp_devices import _default_input_index, resolve_capture_device
from whisper_dictate.vp_events import (
    _audio_level_metrics, _emit_worker_event,
    _sounddevice_capture_channel_candidates, _sounddevice_input_channels,
    _sounddevice_input_name, _sounddevice_stream_kwargs,
)
from whisper_dictate.vp_feedback import notify_error

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


def _max_record_s() -> float:
    """Maximum recording length in seconds; 0 disables the cap.

    Read live from the environment so a config reload takes effect on the next
    recording without a restart (``live: true`` in the schema).
    """
    raw = (os.environ.get("VOICEPI_MAX_RECORD_S") or "120").strip()
    try:
        return float(raw)
    except (ValueError, TypeError):
        return 120.0


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
    """Resolve a VOICEPI_AUDIO_DEVICE value to a ``(device, name)`` pair.

    Resolution prefers the WASAPI→DirectSound→default host API (the SAME rule
    the microphone picker uses, via :func:`vp_devices.resolve_capture_device`) so
    on Windows capture binds the full-name WASAPI device instead of MME's
    31-char-truncated, low-fidelity entry. Linux/macOS expose a single host API,
    so behaviour there is unchanged.

    Value semantics:
      * empty/unset       ⇒ the preferred host API's DEFAULT input device (full
                            name) — never the global MME default — or ``None``.
      * an integer string ⇒ that device index (int), used verbatim.
      * otherwise         ⇒ the matching input device in the preferred host API
                            (full name), tolerating an old MME-truncated saved
                            name (bidirectional-substring match). No match ⇒
                            warn + ``None`` (sounddevice picks the default).

    Returns ``(device, name)`` where ``device`` is an int index or ``None`` and
    ``name`` is the resolved full device name or ``None`` (caller derives a
    label otherwise).
    """
    value = (value or "").strip()
    try:
        devices = sd.query_devices()
    except Exception:
        devices = []
    try:
        hostapis = sd.query_hostapis()
    except Exception:
        hostapis = []
    index, name = resolve_capture_device(
        devices,
        hostapis,
        value,
        is_windows=(os.name == "nt"),
        default_index=_default_input_index(sd),
    )
    if value and not value.lstrip("+-").isdigit() and index is None:
        print(
            f"[cap] audio device {value!r} not found, using default",
            file=sys.stderr,
            flush=True,
        )
    return index, name


def _open_sounddevice_stream(sd, device, callback):
    """Open an ``InputStream`` for ``device``, trying channel/latency fallbacks.

    Returns ``(stream, channels)`` on success (the stream is NOT yet started) or
    ``(None, 0)`` if every channel-candidate × kwargs combination raised. The
    per-channel fallback (``_sounddevice_capture_channel_candidates``) and the
    low-latency→base kwargs fallback (``_sounddevice_stream_kwargs``) are
    preserved exactly as before. ``device=None`` opens the system default.

    Module-level (not a method) so it stays unit-testable and so the caller can
    invoke it twice — once for the preferred device, once for the default
    fallback — without duplicating the channel-fallback loop.
    """
    last_error = None
    for channels in _sounddevice_capture_channel_candidates(_sounddevice_input_channels(sd)):
        for kwargs in _sounddevice_stream_kwargs(channels, callback):
            if device is not None:
                kwargs["device"] = device
            try:
                stream = sd.InputStream(**kwargs)
                return stream, channels
            except Exception as exc:
                last_error = exc
    if last_error is not None:
        print(f"[cap] stream open failed: {last_error}", file=sys.stderr, flush=True)
    return None, 0


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
            # Fix 5: enforce max recording cap in the sounddevice callback.
            cap = _max_record_s()
            if cap > 0:
                total_samples = sum(f.shape[0] for f in self.frames) + chunk.shape[0]
                buffered_s = total_samples / SR
                if buffered_s > cap:
                    if not getattr(self, "_cap_warned", False):
                        self._cap_warned = True
                        print(
                            f"[cap] max recording reached ({cap:.0f}s) — release the key",
                            flush=True,
                        )
                        _emit_worker_event("status", state="recording", capped=True,
                                          recording_s=round(buffered_s, 1))
                    return
            self.frames.append(chunk)
            self._emit_audio_level(chunk)

    def _arecord_reader(self, proc):
        # Read raw S16_LE mono 16kHz from arecord stdout into self.frames
        chunk = SR * 2 * 1  # 1 second of S16 mono = SR*2 bytes
        try:
            while self.recording:
                data = proc.stdout.read(chunk // 8)  # read ~125ms chunks
                if not data:
                    # Fix 4: EOF while still recording means the device was lost.
                    if self.recording:
                        print("[cap] capture lost: arecord EOF while recording", flush=True)
                        _emit_worker_event("status", state="capture_lost",
                                          reason="arecord_eof")
                        notify_error("whisper-dictate", "Capture lost: audio device disconnected")
                    break
                arr = np.frombuffer(data, dtype=np.int16).reshape(-1, 1)
                if not self._first_audio_event.is_set():
                    self._first_audio_at = time.monotonic()
                    self._record_started = self._first_audio_at
                    self._first_audio_event.set()
                # Fix 5: enforce max recording cap in the arecord reader.
                cap = _max_record_s()
                if cap > 0:
                    total_samples = sum(f.shape[0] for f in self.frames) + arr.shape[0]
                    buffered_s = total_samples / SR
                    if buffered_s > cap:
                        if not getattr(self, "_cap_warned", False):
                            self._cap_warned = True
                            print(
                                f"[cap] max recording reached ({cap:.0f}s) — release the key",
                                flush=True,
                            )
                            _emit_worker_event("status", state="recording", capped=True,
                                              recording_s=round(buffered_s, 1))
                        continue
                self.frames.append(arr)
                self._emit_audio_level(arr)
        except Exception as exc:
            # Fix 4: unexpected error in the reader (e.g. device unplugged).
            print(f"[cap] capture lost: {exc}", flush=True)
            _emit_worker_event("status", state="capture_lost", reason=str(exc))
            notify_error("whisper-dictate", f"Capture lost: {exc}")

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
        self._cap_warned = False
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
        self._cap_warned = False
        device, device_name = _resolve_sounddevice_device(sd, _audio_device_setting())
        self._audio_input_device = (
            device_name
            or _selected_device_name(sd, device)
            or _sounddevice_input_name(sd)
            or "sounddevice default input"
        )

        # Try the preferred (WASAPI-resolved) device first. WASAPI selection can
        # fail on some machines/devices, so if opening the explicitly resolved
        # device raises, fall back to the system default (device=None) rather
        # than letting dictation break. Only fall back when an explicit device
        # was chosen — a None device is already the default path.
        self._stream, self._capture_channels = _open_sounddevice_stream(sd, device, self._cb)
        if self._stream is None and device is not None:
            print(
                f"[cap] device {device!r} ({self._audio_input_device!r}) failed to open, "
                "falling back to system default",
                file=sys.stderr,
                flush=True,
            )
            self._stream, self._capture_channels = _open_sounddevice_stream(sd, None, self._cb)
            if self._stream is not None:
                self._audio_input_device = (
                    _sounddevice_input_name(sd) or "sounddevice default input"
                )
        if self._stream is None:
            raise RuntimeError("could not open any sounddevice input stream")
        self._stream.start()
        return self._capture_backend, self._audio_input_device

    def _stop_capture_streams(self) -> None:
        # Fix 1: timeout on arecord wait; kill if it hangs.
        # Fix 2: drain trailing bytes from arecord stdout after terminate/wait.
        # Fix 3: always clear refs even when stop/terminate raises.
        proc = self._arecord_proc
        stream = self._stream
        if proc is not None:
            try:
                proc.terminate()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    try:
                        proc.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        pass
                # Fix 2: drain any remaining bytes written before the pipe closed.
                # Decision: drain always (not just on normal-stop), because
                # _stop_and_transcribe snapshots self.frames after this call and
                # any trailing whole samples should be included. Draining on abort
                # is harmless since _stop_and_transcribe checks self.frames after.
                try:
                    tail = proc.stdout.read()
                    if tail:
                        # Only whole 2-byte int16 samples; drop trailing odd byte.
                        if len(tail) % 2 != 0:
                            tail = tail[:-1]
                        if tail:
                            arr = np.frombuffer(tail, dtype=np.int16).reshape(-1, 1)
                            self.frames.append(arr)
                except Exception as drain_exc:
                    print(f"[cap] drain error (ignored): {drain_exc}", flush=True)
            except Exception as exc:
                print(f"[cap] stop error (ignored): {exc}", flush=True)
            finally:
                self._arecord_proc = None
        if stream is not None:
            try:
                stream.stop()
                stream.close()
            except Exception as exc:
                print(f"[cap] stream stop error (ignored): {exc}", flush=True)
            finally:
                self._stream = None

    def _recording_seconds(self, pcm) -> float:
        if self._record_started:
            return time.monotonic() - self._record_started
        return len(pcm) / SR
