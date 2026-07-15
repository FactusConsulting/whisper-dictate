"""Dry-run microphone test for the ``--test-audio-device`` worker query mode.

The UI's "Test" button (next to the Microphone picker) runs the worker with
``--test-audio-device "<name>"`` so the user can confirm BEFORE starting
dictation whether the mic they picked can actually be opened — and, when it can,
which audio backend/rate/dtype it binds on (so a "works, but via DirectSound at
48 kHz, resampled" is shown honestly rather than silently).

This re-uses the SAME open matrix as live capture (``vp_capture._start_sounddevice``)
— it does NOT re-implement it:

  * device resolution        → :func:`vp_capture._resolve_sounddevice_device`
    (the WASAPI→DirectSound→default host-API rule shared with the picker),
  * the format/rate/dtype sweep on the preferred endpoint, the WASAPI
    ``auto_convert`` retry and the device-native-rate fallback →
    :func:`vp_capture._open_sounddevice_stream` /
    :func:`vp_capture._open_native_rate_stream`,
  * the DirectSound→MME sibling-endpoint fallback →
    :func:`vp_devices.sibling_endpoints_for_device`.

The ONLY difference from live capture is that each opened stream is immediately
stopped and closed — NO audio is captured, nothing is buffered, no long-lived
stream is ever kept. The first candidate that opens wins and its
endpoint/rate/dtype/resampled facts are reported.

Never raises out of :func:`test_audio_device`: any failure (sounddevice missing,
PortAudio error, device not found) is caught and reported as ``usable: false``
with a short ``reason``, so the worker exits cleanly and the UI can render the
result.
"""
from __future__ import annotations

import json
import sys
from typing import Any, Callable

from whisper_dictate import vp_capture
from whisper_dictate.vp_devices import sibling_endpoints_for_device


def _noop_callback(*_args: Any) -> None:
    """A stream callback that drops every frame (dry-run captures nothing)."""


def _close_stream(stream) -> None:
    """Stop + close a just-opened stream so the device is released immediately.

    ``_open_sounddevice_stream`` returns an ALREADY-STARTED stream. The dry run
    only needs to prove it opens, so we stop and close it at once — no frame is
    ever read, nothing is buffered. Fully guarded: a teardown error must not turn
    a successful open into a failure.
    """
    try:
        stream.stop()
    except Exception:  # noqa: BLE001 - teardown must not mask a successful open
        pass
    try:
        stream.close()
    except Exception:  # noqa: BLE001
        pass


def _endpoint_label(hostapi_name: str) -> str:
    """Map a PortAudio host-API name to the short endpoint token the UI shows.

    ``"Windows WASAPI"`` → ``"wasapi"``, ``"Windows DirectSound"`` →
    ``"directsound"``, ``"MME"`` → ``"mme"``; anything else (ALSA / CoreAudio /
    unknown) → ``"default"`` so a non-Windows single-host-API platform still
    reports a sensible endpoint.
    """
    folded = (hostapi_name or "").casefold()
    if "wasapi" in folded:
        return "wasapi"
    if "directsound" in folded:
        return "directsound"
    if "mme" in folded:
        return "mme"
    return "default"


def _result(
    *,
    device: str,
    usable: bool,
    endpoint: str | None = None,
    samplerate: int | None = None,
    dtype: str | None = None,
    resampled: bool = False,
    reason: str | None = None,
) -> dict:
    """Assemble the single JSON result object the ``--test-audio-device`` prints."""
    return {
        "device": device,
        "usable": usable,
        "endpoint": endpoint,
        "samplerate": samplerate,
        "dtype": dtype,
        "resampled": resampled,
        "reason": reason,
    }


def _try_open(
    sd, device, *, native_rate: bool = False, extra_settings=None
) -> tuple[Any, int, str, int]:
    """Dry-run one open candidate; return ``(stream, channels, dtype, rate)``.

    Reuses the SAME open helpers as live capture: ``native_rate`` re-opens at the
    device default rate (resample path) via
    :func:`vp_capture._open_native_rate_stream`; otherwise the 16k int16→float32
    sweep via :func:`vp_capture._open_sounddevice_stream` (optionally with WASAPI
    ``extra_settings``). ``stream`` is ``None`` when nothing opened.
    """
    if native_rate:
        stream, channels, dtype, rate, _exc = vp_capture._open_native_rate_stream(
            sd, device, _noop_callback)
        return stream, channels, dtype, rate
    stream, channels, dtype, _exc = vp_capture._open_sounddevice_stream(
        sd, device, _noop_callback, extra_settings=extra_settings)
    return stream, channels, dtype, (vp_capture.SR if stream is not None else 0)


def _probe_endpoint(sd, device, *, allow_autoconvert: bool) -> dict | None:
    """Dry-run the full open sweep on ONE resolved endpoint (no sibling fallback).

    Mirrors the per-endpoint matrix in ``_start_sounddevice``: 16k int16/float32
    first; on Windows WASAPI the ``auto_convert`` retry; then the device-native
    rate (resample) fallback. Returns the success fact dict (endpoint label is
    filled in by the caller, which knows the host API) or ``None`` if every
    candidate on this endpoint failed. Each opened stream is closed immediately.
    """
    # 16k sweep on the preferred device.
    stream, _ch, dtype, rate = _try_open(sd, device)
    if stream is not None:
        _close_stream(stream)
        return {"samplerate": rate, "dtype": dtype, "resampled": False}

    # WASAPI auto_convert retry (Windows-only; siblings pass allow_autoconvert=False
    # because non-WASAPI endpoints reject WasapiSettings).
    if allow_autoconvert and device is not None and _is_windows_wasapi(sd, device):
        extra = vp_capture._wasapi_autoconvert_settings(sd)
        if extra is not None:
            stream, _ch, dtype, rate = _try_open(sd, device, extra_settings=extra)
            if stream is not None:
                _close_stream(stream)
                # auto_convert resamples 16k internally → resampled is True.
                return {"samplerate": rate, "dtype": dtype, "resampled": True}

    # Device-native-rate fallback (open at the device default rate, resample → 16k).
    stream, _ch, dtype, rate = _try_open(sd, device, native_rate=True)
    if stream is not None:
        _close_stream(stream)
        return {"samplerate": rate, "dtype": dtype, "resampled": True}
    return None


def _is_windows_wasapi(sd, device) -> bool:
    """True when ``device`` is a WASAPI endpoint (so auto_convert is worth trying)."""
    import os

    if os.name != "nt":
        return False
    return vp_capture._is_wasapi_device(
        vp_capture._safe_query_devices(sd),
        vp_capture._safe_query_hostapis(sd),
        device,
    )


def _hostapi_name_for(sd, device) -> str:
    """Best-effort host-API name for a resolved ``device`` index (``""`` if unknown)."""
    if not isinstance(device, int):
        return ""
    return vp_capture._device_hostapi_name(sd, device)


def dry_run_test_device(sd, value: str) -> dict:
    """Resolve ``value`` and dry-run the open matrix; return the result dict.

    Reuses the live-capture resolution + open matrix end to end, opening (and
    immediately closing) each candidate until one succeeds. Order matches
    ``_start_sounddevice``:

      1. resolve the device (WASAPI→DirectSound→default host API),
      2. sweep the resolved endpoint (16k int16/float32 → WASAPI auto_convert →
         native rate),
      3. on failure, the DirectSound→MME sibling endpoints of the SAME physical
         mic (no WasapiSettings — those endpoints reject it).

    ``value`` empty/unset tests the system-default input (``device=None``). A
    name that does not resolve to any device yields ``usable: false`` with
    ``reason: "device not found"``. Never raises — wrapped by
    :func:`test_audio_device`.
    """
    requested = (value or "").strip()
    device, name = vp_capture._resolve_sounddevice_device(sd, requested)
    resolved_label = name or vp_capture._selected_device_name(sd, device) or requested

    # A non-empty, non-numeric name that resolved to nothing == device not found.
    if requested and not requested.lstrip("+-").isdigit() and device is None:
        return _result(device=resolved_label or requested, usable=False,
                       reason="device not found")

    # Probe the resolved (preferred) endpoint with the full sweep incl. WASAPI
    # auto_convert; report it labelled with its real host API.
    success = _probe_endpoint(sd, device, allow_autoconvert=True)
    if success is not None:
        endpoint = _endpoint_label(_hostapi_name_for(sd, device)) if device is not None else "default"
        return _result(
            device=resolved_label or requested or "System default",
            usable=True,
            endpoint=endpoint,
            samplerate=success["samplerate"],
            dtype=success["dtype"],
            resampled=success["resampled"],
        )

    # Sibling-endpoint fallback: retry the SAME physical mic via its
    # DirectSound→MME siblings (no auto_convert — they reject WasapiSettings),
    # exactly as live capture does before surfacing "device unusable".
    if device is not None:
        endpoints = sibling_endpoints_for_device(sd, device)
        for sib_index, hostapi_name in endpoints[1:]:
            success = _probe_endpoint(sd, sib_index, allow_autoconvert=False)
            if success is not None:
                return _result(
                    device=resolved_label or requested or "System default",
                    usable=True,
                    endpoint=_endpoint_label(hostapi_name),
                    samplerate=success["samplerate"],
                    dtype=success["dtype"],
                    resampled=success["resampled"],
                )

    return _result(
        device=resolved_label or requested or "System default",
        usable=False,
        reason="could not open on any audio backend",
    )


def test_audio_device(value: str, *, sd_factory: Callable[[], Any] | None = None) -> int:
    """``--test-audio-device`` entry: dry-run the device and print ONE JSON object.

    Imports sounddevice lazily (never loads a model, never keeps a stream open).
    Prints exactly one JSON object to stdout and returns a process exit code (0
    always — a non-usable device is a normal, reportable outcome, not a worker
    error). ``sd_factory`` is an injection seam for tests; production passes the
    real ``import sounddevice``.

    NEVER raises: a missing sounddevice or any unexpected error becomes
    ``usable: false`` with a short ``reason``.
    """
    requested = (value or "").strip()
    try:
        if sd_factory is not None:
            sd = sd_factory()
        else:
            import sounddevice as sd  # noqa: F401
    except Exception as exc:  # noqa: BLE001 - report cleanly to the caller
        print(json.dumps(_result(
            device=requested or "System default",
            usable=False,
            reason=f"sounddevice unavailable: {exc}",
        ), ensure_ascii=False), flush=True)
        return 0

    try:
        result = dry_run_test_device(sd, requested)
    except Exception as exc:  # noqa: BLE001 - never raise out of the query mode
        print(
            f"[test-device] unexpected error (reported as unusable): {exc}",
            file=sys.stderr, flush=True)
        result = _result(
            device=requested or "System default",
            usable=False,
            reason=f"unexpected error: {exc}",
        )
    print(json.dumps(result, ensure_ascii=False), flush=True)
    return 0
