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

from whisper_dictate.vp_devices import (
    _default_input_index, _is_wasapi_device, resolve_capture_device,
    sibling_endpoints_for_device,
)
from whisper_dictate.vp_events import (
    _audio_level_metrics, _emit_worker_event,
    _sounddevice_capture_channel_candidates, _sounddevice_input_channels,
    _sounddevice_input_name, _sounddevice_stream_kwargs,
)
from whisper_dictate.vp_feedback import notify_error

FIRST_AUDIO_WAIT_S = 0.35

_ARECORD_DEVICE: str | None = None  # set once at startup


class DeviceUnusableError(RuntimeError):
    """An EXPLICITLY-chosen microphone could not be opened on any host API.

    Raised by :meth:`CaptureMixin._start_sounddevice` when the user picked a
    specific input device (``VOICEPI_AUDIO_DEVICE`` non-empty) and every host-API
    endpoint of that physical device — WASAPI (incl. auto_convert + native-rate),
    DirectSound, MME — refused to open. We deliberately do NOT silently record
    from a DIFFERENT physical device (the system default) in this case: that
    would capture the wrong mic while the user speaks into the chosen one.

    ``_handle_capture_start_failure`` surfaces ``message`` verbatim in the
    ``status=error`` event (rather than the generic "format unsupported" wrapper)
    so the UI can show an actionable instruction naming the device. Subclasses
    ``RuntimeError`` so the existing ``except (Exception,)`` crash-safety guard in
    ``Dictate._start`` already catches it — nothing escapes the pynput listener.
    """

    def __init__(self, message: str, device_label: str):
        super().__init__(message)
        self.message = message
        self.device_label = device_label


def _device_unusable_message(device_label: str) -> str:
    """Actionable error text for an explicitly-chosen mic that won't open.

    Names the device and tells the user the concrete next step. Kept as a small
    pure helper so the exact UI-facing string is unit-testable in isolation.
    """
    return (
        f"Microphone {device_label!r} could not be opened on any audio backend "
        "— select a different microphone in Settings."
    )

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


def _trace_enabled() -> bool:
    """Whether maximal ``Trace`` diagnostics are on (env ``VOICEPI_TRACE``).

    Read live (not at import) so a live-reloaded VOICEPI_TRACE takes effect on
    the next recording without a restart — the same pattern as
    :func:`_audio_device_setting` / :func:`_max_record_s`. Trace is purely
    additive: when off, none of the ``[trace]`` lines below are emitted and the
    existing Basic/Verbose output is byte-for-byte unchanged.
    """
    return (os.environ.get("VOICEPI_TRACE") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


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


def _safe_query_devices(sd):
    """``sd.query_devices()`` or ``[]`` — never raises (legacy/stub safe)."""
    try:
        return sd.query_devices()
    except Exception:
        return []


def _safe_query_hostapis(sd):
    """``sd.query_hostapis()`` or ``[]`` — never raises (legacy/stub safe)."""
    try:
        return sd.query_hostapis()
    except Exception:
        return []


def trace_dump_audio_devices() -> None:
    """Log the FULL audio-device enumeration (maximal ``Trace`` diagnostics).

    For every INPUT device prints one ``[trace][devices] …`` line carrying the
    query index, name, host-API name, max_input_channels and default_samplerate,
    so a "why won't my mic open" can be cross-referenced against every capture
    attempt (which host APIs even exist, what rate each device runs at) from the
    log alone. Also prints a one-line host-API summary.

    Called once at startup ONLY when ``VOICEPI_TRACE`` is on (the caller gates).
    Fully guarded: any import / query failure is logged and swallowed so it can
    never raise or block worker startup — a missing audio stack must not stop the
    worker from coming up.
    """
    try:
        import sounddevice as sd
    except Exception as exc:  # noqa: BLE001 - never block startup
        print(f"[trace][devices] sounddevice unavailable (ignored): {exc}",
              file=sys.stderr, flush=True)
        return
    try:
        devices = _safe_query_devices(sd)
        hostapis = _safe_query_hostapis(sd)
        api_names = [
            str(a.get("name") or "?") if isinstance(a, dict) else "?"
            for a in hostapis
        ]
        print(
            f"[trace][devices] host-apis: {api_names or '(none)'}",
            file=sys.stderr, flush=True)
        printed = 0
        for index, info in enumerate(devices):
            if not isinstance(info, dict):
                continue
            try:
                channels = int(info.get("max_input_channels") or 0)
            except (TypeError, ValueError):
                channels = 0
            if channels <= 0:
                continue
            api_index = info.get("hostapi")
            api = (api_names[api_index]
                   if isinstance(api_index, int) and 0 <= api_index < len(api_names)
                   else "?")
            print(
                f"[trace][devices] in dev={index} name={str(info.get('name') or '').strip()!r} "
                f"host={api} max_in_ch={channels} "
                f"default_sr={info.get('default_samplerate')}",
                file=sys.stderr, flush=True)
            printed += 1
        if printed == 0:
            print("[trace][devices] no input devices found",
                  file=sys.stderr, flush=True)
    except Exception as exc:  # noqa: BLE001 - never block startup
        print(f"[trace][devices] enumeration failed (ignored): {exc}",
              file=sys.stderr, flush=True)


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
    devices = _safe_query_devices(sd)
    hostapis = _safe_query_hostapis(sd)
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


def resolve_startup_audio_device() -> str:
    """Resolve the active input-device LABEL at startup WITHOUT opening a stream.

    Called once the worker is ready (before the first recording) so the UI shows
    the active mic from ``status=ready`` instead of a blank "Input pending" until
    the first recording opens the capture stream. Mirrors ``_start``'s backend
    pick: an available PipeWire arecord route yields ``arecord -D <device>`` (as
    ``_start_arecord`` does); otherwise ``VOICEPI_AUDIO_DEVICE`` is resolved via
    :func:`resolve_capture_device` (the SAME pure resolver capture uses — it only
    queries devices, opens no stream) and the full name returned.

    Never raises/blocks: any import/resolve failure degrades to "System default"
    (also used when nothing is configured / nothing resolves). The truly-bound
    device is re-derived when a recording opens the stream (``_start_*``), so a
    "System default" startup label is corrected on the first recording.
    """
    requested = _audio_device_setting()
    arecord_device = _ensure_arecord_device()
    if arecord_device:
        device = _arecord_device_arg(arecord_device, requested)
        return f"arecord -D {device}"
    try:
        import sounddevice as sd
        _index, name = _resolve_sounddevice_device(sd, requested)
        if name:
            return name
        if _index is not None:
            label = _selected_device_name(sd, _index)
            if label:
                return label
            return _sounddevice_input_name(sd) or "System default"
        # No explicit device and nothing resolved → the OS default input.
        return _sounddevice_input_name(sd) or "System default"
    except Exception as exc:
        print(
            f"[cap] could not resolve startup audio device (ignored): {exc}",
            file=sys.stderr,
            flush=True,
        )
        return "System default"


def _wasapi_autoconvert_settings(sd):
    """Return ``sd.WasapiSettings(auto_convert=True)`` or ``None`` if unavailable.

    Lets a WASAPI device resample our requested 16k internally on machines whose
    shared-mode native rate is 48k and that reject a raw 16k open. Older
    sounddevice builds lack ``WasapiSettings`` (and a non-WASAPI device would
    reject the setting), so this is fully guarded — any failure yields ``None``
    and the caller simply skips the auto-convert candidate.
    """
    factory = getattr(sd, "WasapiSettings", None)
    if factory is None:
        return None
    try:
        return factory(auto_convert=True)
    except Exception:
        return None


def _device_default_samplerate(sd, device) -> int | None:
    """The device's native/default capture rate (Hz) or ``None`` if unknown.

    Used by the native-rate fallback: when a 16k open is rejected (e.g. a Yeti /
    Focusrite / webcam mic running natively at 44.1/48 kHz) we re-open at the
    rate the device actually runs at and resample to 16k in software. Queries
    ``sd.query_devices(device)['default_samplerate']`` (or the default input
    device's when ``device`` is ``None``). Never raises — any failure yields
    ``None`` and the caller picks a sensible fallback rate.
    """
    try:
        if device is None:
            info = sd.query_devices(kind="input")
        else:
            info = sd.query_devices(device)
        if not isinstance(info, dict):
            return None
        rate = info.get("default_samplerate")
        if rate is None:
            return None
        rate = int(round(float(rate)))
        return rate if rate > 0 else None
    except Exception:
        return None


def _resample_capture_buffer(pcm, capture_rate: int):
    """Resample a mono int16 capture buffer from ``capture_rate`` to ``SR``.

    Pure helper (extracted so the line-length rule stays satisfied and so the
    resample is unit-testable in isolation). ``pcm`` is an int16 array shaped
    ``(N,)`` or ``(N, 1)``; the result is int16 shaped ``(M, 1)`` at the model's
    16k rate. A no-op (returns an int16 ``(N, 1)`` view) when ``capture_rate`` is
    already ``SR`` or falsy — current behaviour for 16k-native devices is
    bit-identical. Reuses the existing :func:`vp_audio_file._resample_mono` /
    :func:`vp_audio_file._mono_float_to_int16` so no new resampling dependency is
    introduced.
    """
    audio = np.asarray(pcm)
    if audio.ndim > 1:
        audio = audio.reshape(-1)
    if not capture_rate or capture_rate == SR or len(audio) == 0:
        # No-resample fast path: avoid a needless copy when the buffer is already
        # int16 (the common 16k-native case). `copy=False` returns the same
        # underlying data when no cast is required; `.reshape(-1, 1)` then yields
        # a view, preserving the (N, 1) shape contract without an extra copy.
        return audio.astype(np.int16, copy=False).reshape(-1, 1)
    from whisper_dictate.vp_audio_file import _mono_float_to_int16, _resample_mono

    floats = audio.astype(np.float32) / 32768.0
    resampled = _resample_mono(floats, int(capture_rate))
    return _mono_float_to_int16(resampled)


# dtype candidates, in attempt order. int16 is tried first so the current
# happy path (16k-native int16 comms headsets like the Jabra) binds on the very
# FIRST candidate, bit-identically to before the float32 dimension was added.
# float32 is the fallback for devices whose WASAPI shared mixformat is float32
# (e.g. the Blue Yeti = 48k float32) which reject a raw int16 open with
# AUDCLNT_E_UNSUPPORTED_FORMAT; their frames are converted to int16 at capture.
_CAPTURE_DTYPES = ("int16", "float32")


def _capture_frame_to_int16(chunk, dtype):
    """Coerce a sounddevice callback frame to the int16 the buffer expects.

    The frame buffer (``self.frames``) and ALL downstream consumption assume
    int16. When the stream opened as ``float32`` (a device whose WASAPI shared
    mixformat is float32 and that rejected an int16 open) the callback receives
    float32 frames in [-1.0, 1.0]; clip and scale them to int16 here so nothing
    downstream changes. For an int16 stream this is a bit-identical no-op (a
    plain ``.copy()`` of the frame, exactly as before this dtype dimension).

    Pure module-level helper so the float32→int16 conversion is unit-testable in
    isolation and the callback stays small.
    """
    is_float = dtype == "float32" or str(getattr(getattr(chunk, "dtype", None), "name", "")) \
        .startswith("float")
    if is_float:
        clipped = np.clip(chunk, -1.0, 1.0)
        return (clipped * 32767.0).astype(np.int16)
    return chunk.copy()


def _device_hostapi_name(sd, device) -> str:
    """Best-effort host-API name for a resolved ``device=`` arg (Trace only).

    Returns e.g. ``"Windows WASAPI"`` / ``"MME"`` / ``"ALSA"`` so a Trace line
    makes the host API that rejected the format obvious — the key insight when a
    mic won't open (if WASAPI fails every format, the log shows it so we know to
    try MME/DirectSound). Never raises: any query failure yields ``"?"`` and the
    default device (``device is None``) reports the default input's host API.
    """
    try:
        if device is None:
            info = sd.query_devices(kind="input")
        else:
            info = sd.query_devices(device)
        if not isinstance(info, dict):
            return "?"
        api_index = info.get("hostapi")
        if api_index is None:
            return "?"
        hostapis = _safe_query_hostapis(sd)
        if 0 <= int(api_index) < len(hostapis):
            api = hostapis[int(api_index)]
            if isinstance(api, dict):
                return str(api.get("name") or "?").strip() or "?"
    except Exception:
        return "?"
    return "?"


def _open_sounddevice_stream(sd, device, callback, *, extra_settings=None,
                             samplerate=None, trace=False):
    """Open an ``InputStream`` for ``device``, trying dtype/channel/latency fallbacks.

    Returns ``(stream, channels, dtype, last_error)``: ``(stream, channels,
    dtype, None)`` on success (the stream is started) or ``(None, 0, "", exc)``
    with the last PortAudio error if every dtype × channel-candidate × kwargs
    combination raised (``None`` error only when there were no candidates).
    ``dtype`` is the numpy dtype string the stream actually opened with
    (``"int16"`` or ``"float32"``) so the callback can convert float32 frames to
    int16. ``device=None`` opens the system default.

    Attempt order (outer→inner): dtype (int16 then float32) × channel-candidate
    (``_sounddevice_capture_channel_candidates``: native→2→1) × kwargs
    (``_sounddevice_stream_kwargs``: low-latency then base). int16 is fully
    exhausted across all channels/latencies before float32 is tried, so a device
    that accepts int16 binds on its first candidate exactly as before — the
    float32 dimension is invisible to it.

    ``extra_settings`` (e.g. ``sd.WasapiSettings(auto_convert=True)``) is passed
    to every candidate when not ``None`` so the WASAPI auto-convert attempt
    reuses the same fallback matrix.

    ``samplerate`` overrides the requested capture rate (default ``SR``=16k).
    Used by the native-rate fallback: pro-audio / webcam mics that reject a 16k
    open are opened at their device default rate and the buffer is resampled to
    16k at consumption (see ``_resample_capture_buffer``).

    The returned stream is already STARTED. ``.start()`` is called INSIDE the same
    guarded try as ``InputStream(...)`` so a start-time failure (the Yeti / WASAPI
    ``AUDCLNT_E_UNSUPPORTED_FORMAT`` surfaces on *start*, not open — PaErrorCode
    -9999) is handled identically to an open failure: the half-open stream is
    closed and the next dtype/channel/latency/rate candidate (and ultimately the
    native-rate fallback) is tried instead of crashing the worker.

    Module-level (not a method) so it stays unit-testable and so the caller can
    invoke it multiple times — preferred device, WASAPI auto-convert, default
    fallback — without duplicating the fallback loop.

    ``trace`` (maximal ``Trace`` diagnostics) logs ONE ``[trace][cap] attempt …``
    line per candidate — host-API, device index+name, samplerate, channels,
    dtype, auto_convert flag and the per-attempt result (``ok`` or the exact
    exception message) — and the finally-bound candidate, so a "why won't my mic
    open" is diagnosable from the log alone. Off by default: every existing
    Basic/Verbose code path is byte-for-byte unchanged.
    """
    last_error = None
    channel_candidates = _sounddevice_capture_channel_candidates(
        _sounddevice_input_channels(sd))
    hostapi = _device_hostapi_name(sd, device) if trace else None
    dev_label = "default" if device is None else device
    autoconv = 1 if extra_settings is not None else 0
    for dtype in _CAPTURE_DTYPES:
        for channels in channel_candidates:
            for kwargs in _sounddevice_stream_kwargs(channels, callback, samplerate, dtype):
                if device is not None:
                    kwargs["device"] = device
                if extra_settings is not None:
                    kwargs["extra_settings"] = extra_settings
                rate = kwargs.get("samplerate")
                latency = kwargs.get("latency", "base")
                # Bind to None up front so the except's cleanup can tell whether
                # the stream was actually created: if InputStream(...) raises, the
                # name stays None and we skip close() instead of hitting an
                # UnboundLocalError (previously swallowed, but unclean).
                stream = None
                try:
                    stream = sd.InputStream(**kwargs)
                    stream.start()
                    if trace:
                        print(
                            f"[trace][cap] attempt host={hostapi} dev={dev_label} "
                            f"rate={rate} ch={channels} dtype={dtype} "
                            f"latency={latency} autoconv={autoconv} -> ok",
                            file=sys.stderr, flush=True)
                        print(
                            f"[trace][cap] BOUND host={hostapi} dev={dev_label} "
                            f"rate={rate} ch={channels} dtype={dtype} "
                            f"latency={latency} autoconv={autoconv}",
                            file=sys.stderr, flush=True)
                    return stream, channels, dtype, None
                except Exception as exc:
                    last_error = exc
                    if trace:
                        print(
                            f"[trace][cap] attempt host={hostapi} dev={dev_label} "
                            f"rate={rate} ch={channels} dtype={dtype} "
                            f"latency={latency} autoconv={autoconv} -> {exc}",
                            file=sys.stderr, flush=True)
                    # A stream that opened but failed to start must be closed so
                    # the device is released before the next candidate is tried.
                    # Guard on `stream is not None`: if InputStream(...) itself
                    # raised, there is nothing to close.
                    if stream is not None:
                        try:
                            stream.close()
                        except Exception:
                            pass
    if last_error is not None:
        print(f"[cap] stream open failed: {last_error}", file=sys.stderr, flush=True)
    return None, 0, "", last_error


def _open_native_rate_stream(sd, device, callback):
    """Open ``device`` at its native default rate (16k-rejected fallback).

    Returns ``(stream, channels, dtype, rate, error)``: a started stream opened
    at the device's default rate (which the caller resamples native→SR at
    consumption), or ``(None, 0, "", 0, error)``. ``(None, 0, "", 0, None)`` is
    returned without trying when the native rate is unknown or already SR
    (nothing new to attempt). Pure module-level helper so it stays unit-testable
    like ``_open_sounddevice_stream``.
    """
    native = _device_default_samplerate(sd, device)
    if not native or native == SR:
        return None, 0, "", 0, None
    stream, channels, dtype, exc = _open_sounddevice_stream(
        sd, device, callback, samplerate=native, trace=_trace_enabled())
    if stream is not None:
        return stream, channels, dtype, native, None
    return None, 0, "", 0, exc


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
            # Coerce to int16 (a no-op copy for an int16 stream; clip+scale for a
            # float32 stream) so the frame buffer + all downstream code, which
            # assume int16, are unchanged regardless of the opened stream dtype.
            chunk = _capture_frame_to_int16(indata, getattr(self, "_capture_dtype", "int16"))
            # Fix 5: enforce max recording cap in the sounddevice callback.
            cap = _max_record_s()
            if cap > 0:
                # Use the actual capture rate (may be the device-native rate when
                # 16k was rejected) so the duration cap stays accurate.
                rate = getattr(self, "_capture_rate", SR) or SR
                total_samples = sum(f.shape[0] for f in self.frames) + chunk.shape[0]
                buffered_s = total_samples / rate
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
        # arecord is forced to S16_LE mono at SR (16k), so no resampling needed.
        self._capture_rate = SR
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

    def _bind_stream(self, sd, device, *, native_rate=False, extra_settings=None):
        """Open ``device`` and record the bound stream/channels/dtype/rate.

        Thin wrapper around the module-level open helpers that stores the result
        on ``self`` (``_stream`` / ``_capture_channels`` / ``_capture_dtype`` /
        ``_capture_rate``) and returns the last PortAudio error (or ``None`` on
        success / nothing-to-try). ``native_rate=True`` uses
        ``_open_native_rate_stream`` (re-opens at the device default rate and
        records it for the native→16k resample); otherwise opens at SR (16k).
        Keeps ``_start_sounddevice`` flat and short despite the dtype dimension.
        """
        if native_rate:
            stream, channels, dtype, rate, exc = _open_native_rate_stream(
                sd, device, self._cb)
            if stream is not None:
                self._stream = stream
                self._capture_channels = channels
                self._capture_dtype = dtype
                self._capture_rate = rate
                label = self._audio_input_device if device is not None else "default input"
                print(
                    f"[cap] opened {label!r} at native {rate} Hz ({dtype}); "
                    f"resampling to {SR} Hz",
                    file=sys.stderr, flush=True)
            return exc
        stream, channels, dtype, exc = _open_sounddevice_stream(
            sd, device, self._cb, extra_settings=extra_settings,
            trace=_trace_enabled())
        if stream is not None:
            self._stream = stream
            self._capture_channels = channels
            self._capture_dtype = dtype
        return exc

    def _try_sibling_endpoints(self, sd, device):
        """Open the SAME physical mic via its DirectSound/MME siblings.

        The core host-API-fallback fix: when the resolved (WASAPI) endpoint of
        the user's chosen mic refuses to open across the whole format matrix
        (incl. auto_convert + native-rate), retry the SAME physical device on its
        OTHER host APIs — DirectSound first (cheapest, accepts 16k int16 directly,
        no resample), then MME — BEFORE dropping to a DIFFERENT physical device.

        CRITICAL: non-WASAPI endpoints reject ``WasapiSettings`` (PortAudio
        ``-9984 Incompatible host API specific stream info``), so the sibling
        attempts pass NO ``extra_settings`` — just the plain 16k int16→float32
        sweep, then a native-rate sweep. The probe proves MME/DirectSound open a
        Yeti at 16k int16 with neither auto_convert nor resampling, so the first
        16k-capable sibling wins with the cheapest possible path.

        Returns a ``(bound, last_error)`` tuple. On success ``self._stream`` (+
        channels/dtype/rate) is set and ``(True, None)`` is returned; the bound
        label stays the user's full chosen name (it IS that physical device —
        NOT a swap to a different mic). When every sibling endpoint fails,
        ``(False, last_error)`` is returned (caller then drops to the system
        default and surfaces a real device swap) — the last PortAudio error is
        threaded out so the caller can fold it into the final RuntimeError.
        """
        last_error = None
        endpoints = sibling_endpoints_for_device(sd, device)
        trace = _trace_enabled()
        # endpoints[0] is the already-tried resolved endpoint; skip it.
        for sib_index, hostapi_name in endpoints[1:]:
            if trace:
                print(
                    f"[trace][cap] sibling-fallback host={hostapi_name or '?'} "
                    f"dev={sib_index} (same physical device as {device})",
                    file=sys.stderr, flush=True)
            # NO extra_settings — WasapiSettings is WASAPI-only; MME/DS reject it.
            exc = self._bind_stream(sd, sib_index)
            if exc is not None:
                last_error = exc
            if self._stream is None:
                exc = self._bind_stream(sd, sib_index, native_rate=True)
                if exc is not None:
                    last_error = exc
            if self._stream is not None:
                print(
                    f"[cap] opened {self._audio_input_device!r} via its "
                    f"{hostapi_name or 'alternate'} endpoint "
                    f"(WASAPI endpoint refused every format)",
                    file=sys.stderr, flush=True)
                return True, None
        return False, last_error

    def _start_sounddevice(self) -> tuple[str, str]:
        import sounddevice as sd
        self._capture_backend = "sounddevice"
        self._cap_warned = False
        # Capture rate the buffer is recorded at. Defaults to SR (16k); bumped to
        # the device-native rate only when a 16k open is rejected (see below).
        # The buffer is resampled capture_rate→SR at consumption.
        self._capture_rate = SR
        # dtype the stream actually opened with. int16 by default; float32 for
        # devices whose WASAPI shared mixformat is float32 (the callback converts
        # float32 frames → int16 so downstream stays int16). See _capture_frame_to_int16.
        self._capture_dtype = "int16"
        device, device_name = _resolve_sounddevice_device(sd, _audio_device_setting())
        # Explicit-vs-default line: a NON-EMPTY VOICEPI_AUDIO_DEVICE means the user
        # deliberately picked a microphone. When such a device fails on every host
        # API we must NOT silently record from a DIFFERENT physical device — we
        # surface an honest error instead (see the all-endpoints-failed branch).
        # An empty setting (system default) keeps the graceful default fallback.
        explicit_device = bool(_audio_device_setting())
        # The device the USER asked for (full name), so a fallback to a DIFFERENT
        # physical device can be detected + surfaced instead of silently swapping.
        requested_name = (
            device_name or _selected_device_name(sd, device)) if device is not None else None
        self._audio_input_device = (
            device_name
            or _selected_device_name(sd, device)
            or _sounddevice_input_name(sd)
            or "sounddevice default input"
        )

        # Try the preferred (WASAPI-resolved) device first. The open helper now
        # sweeps the full format matrix on this device — (16k int16 native→2→1
        # ch, low→base latency) then (16k float32) — so the Jabra-style happy
        # path still binds on the very first candidate, while a Yeti-style device
        # whose shared mixformat is float32 binds on a float32 candidate.
        last_error = self._bind_stream(sd, device)
        if self._stream is None and device is not None:
            # WASAPI robustness: some WASAPI devices natively run at 48k and
            # reject a raw 16k shared-mode open even after the int16+float32
            # sweep. Before dropping to the system default (the low-fidelity MME
            # path on Windows), let WASAPI resample 16k internally via
            # auto_convert (int16+float32 swept again). Windows-only and guarded.
            if os.name == "nt" and _is_wasapi_device(_safe_query_devices(sd),
                                                      _safe_query_hostapis(sd), device):
                extra = _wasapi_autoconvert_settings(sd)
                if extra is not None:
                    exc = self._bind_stream(sd, device, extra_settings=extra)
                    if exc is not None:
                        last_error = exc
            # Native-rate fallback: pro-audio / webcam mics (Yeti, Focusrite, …)
            # run natively at 44.1/48 kHz and reject a forced 16k open. Open at
            # the device's default rate (int16+float32 swept) and resample to 16k
            # in software. Tried on the CONFIGURED device — exhausting every
            # format — BEFORE dropping to the system default so the user's chosen
            # mic actually works rather than silently swapping mics.
            if self._stream is None:
                exc = self._bind_stream(sd, device, native_rate=True)
                if exc is not None:
                    last_error = exc
            if self._stream is None:
                # Host-API fallback (the core fix): the resolved (WASAPI) endpoint
                # refused EVERY format — incl. auto_convert + native-rate. Retry
                # the SAME physical mic via its DirectSound→MME siblings (no
                # WasapiSettings — those endpoints reject it; 16k int16 opens
                # directly with no resample) BEFORE swapping to a different mic.
                _, exc = self._try_sibling_endpoints(sd, device)
                if exc is not None:
                    last_error = exc
            if self._stream is None and explicit_device:
                # HONEST "device unusable" (Fix 1): the user deliberately chose
                # THIS microphone and every host-API endpoint of it refused to
                # open. Recording from a different physical device (the system
                # default) would silently capture the WRONG mic while the user
                # speaks into the selected one. Abort with an actionable error the
                # UI can show instead. DeviceUnusableError subclasses RuntimeError,
                # so Dictate._start's crash-safety guard catches it, emits a
                # status=error, and leaves the worker idle + ready for the next
                # PTT once the user picks another microphone.
                label = requested_name or self._audio_input_device
                message = _device_unusable_message(label)
                print(
                    f"[cap] device {device!r} ({label!r}) failed to open on every "
                    "host API; explicit selection — NOT swapping to the system "
                    "default. Surfacing an error.",
                    file=sys.stderr,
                    flush=True,
                )
                raise DeviceUnusableError(message, label)
            if self._stream is None:
                # No explicit device was chosen (system default): the default's
                # preferred-host-API endpoint failed every format. Fall back to
                # the OS default so a genuinely-vanished device still degrades
                # gracefully. This is NOT a wrong-mic swap — nothing the user
                # picked is being abandoned — so _note_device_swap stays silent.
                print(
                    f"[cap] device {device!r} ({self._audio_input_device!r}) failed to open "
                    "on every host API, falling back to system default",
                    file=sys.stderr,
                    flush=True,
                )
                exc = self._bind_stream(sd, None)
                if exc is not None:
                    last_error = exc
                if self._stream is None:
                    exc = self._bind_stream(sd, None, native_rate=True)
                    if exc is not None:
                        last_error = exc
                if self._stream is not None:
                    self._note_device_swap(
                        requested_name,
                        _sounddevice_input_name(sd) or "sounddevice default input")
        if self._stream is None and device is None:
            # No explicit device: the system default itself rejected a 16k open.
            # Try it at its native rate before giving up (mirrors the explicit-
            # device native-rate fallback above).
            exc = self._bind_stream(sd, None, native_rate=True)
            if exc is not None:
                last_error = exc
        if self._stream is None:
            detail = f": {last_error}" if last_error is not None else ""
            raise RuntimeError(f"could not open any sounddevice input stream{detail}")
        # The stream is already started inside _open_sounddevice_stream so that a
        # start-time failure is caught + falls through the open/native-rate
        # fallbacks rather than escaping here (and out of the PTT listener).
        return self._capture_backend, self._audio_input_device

    def _note_device_swap(self, requested_name, actual_name) -> None:
        """Record the bound device name when degrading to the system default.

        Reached ONLY in the NO-EXPLICIT-device path (Fix 1): when the user did
        NOT pick a specific mic and the default's preferred-host-API endpoint
        failed every format, capture falls back to the OS default. That is not a
        wrong-mic swap — nothing the user chose is abandoned — so this just sets
        the bound ``audio_device`` label and stays silent.

        When an EXPLICIT device is chosen, every-endpoint failure no longer
        reaches here at all: ``_start_sounddevice`` raises ``DeviceUnusableError``
        BEFORE any different-physical-device fallback (see that branch), so the
        worker never records the wrong microphone. The WARN branch below is thus
        a defensive safety net only (``requested_name`` is the default's own
        name in the path that reaches this method, so it normally no-ops).
        """
        actual = actual_name or "sounddevice default input"
        if not requested_name or requested_name == actual:
            self._audio_input_device = actual
            return
        self._audio_input_device = (
            f"{actual} (WARN: selected {requested_name!r} could not be opened)"
        )
        print(
            f"[cap] WARN selected microphone {requested_name!r} could not be opened; "
            f"recording from {actual!r} instead",
            file=sys.stderr, flush=True)
        _emit_worker_event(
            "status",
            state="recording",
            audio_device=self._audio_input_device,
            device_swap=f"WARN selected {requested_name} unavailable; using {actual}",
        )

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
