"""Runtime event plumbing + live-capture support helpers.

Extracted from runtime.py. Groups the bits the push-to-talk loop and the
file/calibration paths share but that are not the loop itself:

  * utterance/JSON/worker event construction + emission,
  * sounddevice input-device probing and capture-stream kwargs,
  * audio-level metering for the live meter,
  * the Rust command-hook + profile-resolution bridges,
  * the Rust model-capacity printer.

numpy / sounddevice / SR are imported lazily inside the functions so importing
this module stays cheap (``--help`` / ``--doctor`` must not pull in heavy deps).
"""
from __future__ import annotations

import json
import math
import os
import subprocess
import sys
import time

from whisper_dictate.vp_rust import _rust_helper, _rust_json

SOUNDDEVICE_START_BLOCK_MS = 20


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _compact_text(text: str, limit: int = 240) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 3] + "..."


def _base_event(**fields):
    event = {"ts": time.time()}
    event.update(fields)
    return event


def _emit_json(event: dict) -> None:
    print(json.dumps(event, ensure_ascii=False, sort_keys=True), flush=True)


def _emit_worker_event(event: str, **fields) -> None:
    if not _truthy(os.environ.get("VOICEPI_WORKER_EVENTS")):
        return
    payload = {"event": event}
    payload.update({key: value for key, value in fields.items() if value is not None})
    print(
        "[worker-event] "
        + json.dumps(payload, ensure_ascii=True, sort_keys=True, separators=(",", ":")),
        file=sys.stderr,
        flush=True,
    )


def _sounddevice_input_info(sd) -> dict | None:
    try:
        default_device = getattr(getattr(sd, "default", None), "device", None)
        input_device = None
        if isinstance(default_device, (list, tuple)) and default_device:
            input_device = default_device[0]
        elif isinstance(default_device, int):
            input_device = default_device

        if input_device is None or input_device == -1:
            info = sd.query_devices(kind="input")
        else:
            info = sd.query_devices(input_device)
        if isinstance(info, dict):
            return info
    except Exception:
        return None
    return None


def _default_input_index(sd) -> int | None:
    """The index of sounddevice's default input device, or ``None``.

    Mirrors the default-device resolution used elsewhere: ``sd.default.device``
    is either ``(input, output)`` or a single int; a value of ``-1`` means "no
    explicit default".
    """
    default_device = getattr(getattr(sd, "default", None), "device", None)
    if isinstance(default_device, (list, tuple)) and default_device:
        candidate = default_device[0]
    elif isinstance(default_device, int):
        candidate = default_device
    else:
        return None
    if not isinstance(candidate, int) or candidate < 0:
        return None
    return candidate


def list_input_devices(sd) -> list[dict]:
    """Return input devices (max_input_channels > 0) for the device picker.

    Each entry is ``{"index", "name", "max_input_channels", "default"}``. Pure
    over an injected ``sd`` so it is unit-testable with a stubbed sounddevice.
    """
    default_index = _default_input_index(sd)
    devices = sd.query_devices()
    result: list[dict] = []
    for index, info in enumerate(devices):
        if not isinstance(info, dict):
            continue
        try:
            channels = int(info.get("max_input_channels") or 0)
        except (TypeError, ValueError):
            channels = 0
        if channels <= 0:
            continue
        name = str(info.get("name") or "").strip()
        if not name:
            # An empty name would collide with the UI's "" = "(System default)"
            # combo value and make the selection ambiguous — skip the entry.
            continue
        result.append({
            "index": index,
            "name": name,
            "max_input_channels": channels,
            "default": index == default_index,
        })
    return result


def print_windows() -> int:
    """Print the list of visible top-level windows as JSON and return an exit code.

    On Windows prints a JSON array of ``{"title": "...", "process": "..."}``
    objects and returns 0.  On non-Windows platforms (Wayland cannot enumerate
    windows; X11 support is deferred) prints ``{"error": "..."}`` and returns 1.
    """
    if os.name != "nt":
        print(json.dumps({"error": "window listing is only supported on Windows"}), flush=True)
        return 1
    try:
        from whisper_dictate.vp_windows import list_visible_windows
        windows = list_visible_windows()
    except Exception as exc:  # noqa: BLE001
        print(json.dumps({"error": f"could not enumerate windows: {exc}"}), flush=True)
        return 1
    print(json.dumps(windows, ensure_ascii=False), flush=True)
    return 0


def print_audio_devices() -> int:
    """Print the input-device list as JSON and return a process exit code.

    Cheap: imports sounddevice lazily and never loads a model or opens a stream.
    On success prints a JSON array and returns 0; if sounddevice is unavailable
    (or querying fails) prints ``{"error": "..."}`` and returns 1.
    """
    try:
        import sounddevice as sd
    except Exception as exc:  # noqa: BLE001 - report cleanly to the caller
        print(json.dumps({"error": f"sounddevice unavailable: {exc}"}), flush=True)
        return 1
    try:
        devices = list_input_devices(sd)
    except Exception as exc:  # noqa: BLE001 - report cleanly to the caller
        print(json.dumps({"error": f"could not query audio devices: {exc}"}), flush=True)
        return 1
    print(json.dumps(devices, ensure_ascii=False), flush=True)
    return 0


def _sounddevice_input_name(sd) -> str | None:
    info = _sounddevice_input_info(sd)
    if not info:
        return None
    name = str(info.get("name") or "").strip()
    return name or None


def _sounddevice_input_channels(sd) -> int:
    info = _sounddevice_input_info(sd)
    if not info:
        return 1
    try:
        channels = int(info.get("max_input_channels") or 1)
    except (TypeError, ValueError):
        return 1
    return max(1, channels)


def _sounddevice_capture_channel_candidates(max_channels: int) -> list[int]:
    max_channels = max(1, min(8, int(max_channels or 1)))
    candidates = [max_channels]
    for fallback in (2, 1):
        if fallback <= max_channels and fallback not in candidates:
            candidates.append(fallback)
    return candidates


def _sounddevice_stream_kwargs(channels: int, callback) -> list[dict]:
    from whisper_dictate.vp_transcribe import SR

    base = {
        "samplerate": SR,
        "channels": channels,
        "dtype": "int16",
        "callback": callback,
    }
    low_latency = dict(base)
    low_latency.update({
        "blocksize": max(1, int(SR * SOUNDDEVICE_START_BLOCK_MS / 1000)),
        "latency": "low",
    })
    return [low_latency, base]


def _audio_meter_level_from_dbfs(raw_dbfs: float) -> float:
    try:
        raw = float(raw_dbfs)
    except (TypeError, ValueError):
        return 0.0
    if math.isnan(raw):
        return 0.0
    floor = -60.0
    ceiling = -12.0
    clamped = min(ceiling, max(floor, raw))
    normalized = (clamped - floor) / (ceiling - floor)
    return float(normalized ** 1.4)


def _select_active_channel_pcm(pcm):
    import numpy as np

    audio = np.asarray(pcm)
    if audio.ndim == 0:
        return audio.reshape(1, 1)
    if audio.ndim == 1:
        return audio.reshape(-1, 1)
    if audio.ndim > 2:
        audio = audio.reshape(audio.shape[0], -1)
    if audio.shape[1] <= 1:
        return audio.reshape(-1, 1)

    levels = audio.astype(np.float32)
    if getattr(audio, "dtype", None) is not None and audio.dtype.kind in ("i", "u"):
        levels = levels / 32768.0
    rms_by_channel = np.sqrt(np.mean(levels ** 2, axis=0))
    active_channel = int(np.argmax(rms_by_channel))
    return audio[:, active_channel:active_channel + 1]


def _audio_level_metrics(pcm) -> tuple[float, float, float]:
    import numpy as np

    mono = _select_active_channel_pcm(pcm)
    audio = mono.reshape(-1).astype(np.float32)
    if len(audio) == 0:
        return -120.0, 0.0, 0.0
    if getattr(mono, "dtype", None) is not None and mono.dtype.kind in ("i", "u"):
        audio = audio / 32768.0
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    rms = float(np.sqrt(np.mean(audio ** 2)) or 1e-9)
    raw_dbfs = float(20 * np.log10(max(rms, 1e-9)))
    return raw_dbfs, peak, _audio_meter_level_from_dbfs(raw_dbfs)


def _run_command_hook_and_annotate(event: dict) -> None:
    result = _rust_json(
        "command-hook",
        event,
        timeout=max(
            1.0,
            float(os.environ.get("VOICEPI_COMMAND_HOOK_TIMEOUT_MS") or "2000") / 1000.0 + 1.0,
        ),
    )
    result = result or {
        "enabled": False,
        "command": "",
        "returncode": None,
        "latency_ms": 0,
        "timeout": False,
        "error": None,
    }
    event.update({
        "command_hook_enabled": bool(result.get("enabled", False)),
        "command_hook_command": result.get("command") or None,
        "command_hook_returncode": result.get("returncode"),
        "command_hook_latency_ms": int(result.get("latency_ms") or 0),
        "command_hook_timeout": bool(result.get("timeout", False)),
        "command_hook_error": result.get("error"),
    })


def _apply_profile_settings(base: dict[str, str], profiles, *, title: str | None, process: str | None):
    result = _rust_json("apply-profile", {
        "base": base,
        "profiles": profiles,
        "title": title,
        "process": process,
    })
    if not result:
        return dict(base), None
    config = result.get("config", {})
    if not isinstance(config, dict):
        return dict(base), None
    name = result.get("name")
    return {str(key): str(value) for key, value in config.items()}, str(name) if name else None


def _print_model_capacity(as_json: bool) -> bool:
    helper = _rust_helper()
    if not helper:
        return False
    args = [helper, "model-capacity"]
    if as_json:
        args.append("--json")
    try:
        r = subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=5)
    except Exception as e:  # noqa: BLE001
        print(f"[model-capacity] {e}", file=sys.stderr, flush=True)
        return False
    if r.returncode != 0:
        err = (r.stderr or "").strip()
        if err:
            print(f"[model-capacity] {err}", file=sys.stderr, flush=True)
        return False
    print((r.stdout or "").rstrip("\n"), flush=True)
    return True
