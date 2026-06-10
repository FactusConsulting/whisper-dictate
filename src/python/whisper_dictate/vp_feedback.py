"""Audible cues and desktop notifications for headless/autostart usage.

When whisper-dictate runs with ``Terminal=false`` (e.g. a GNOME autostart
entry), all console output is swallowed. The two settings in this module
surface two optional signal types that survive without a visible terminal:

* ``VOICEPI_FEEDBACK_SOUNDS`` — short audio cue on record start/stop.
* ``VOICEPI_FEEDBACK_NOTIFY`` — desktop notification on errors.

Both default to *off* so existing users are unaffected.

Design principles
-----------------
* Every failure is swallowed — a broken audio subsystem must never add
  latency to dictation or prevent the app from starting.
* Cues are **non-blocking**: subprocess calls use ``Popen`` with
  ``DEVNULL`` I/O so the caller returns immediately.
* Settings are read **live** (via ``get_value``) on each call so that a
  live-reload of ``config.json`` takes effect without a restart.
"""
from __future__ import annotations

import os
import subprocess
import sys

# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _sounds_enabled() -> bool:
    """Return True when VOICEPI_FEEDBACK_SOUNDS is set to a truthy value."""
    try:
        from whisper_dictate.vp_config import get_value
        v = get_value("VOICEPI_FEEDBACK_SOUNDS") or ""
    except Exception:
        v = os.environ.get("VOICEPI_FEEDBACK_SOUNDS") or ""
    return v.lower() not in ("", "0", "false", "no", "off")


def _notify_enabled() -> bool:
    """Return True when VOICEPI_FEEDBACK_NOTIFY is set to a truthy value."""
    try:
        from whisper_dictate.vp_config import get_value
        v = get_value("VOICEPI_FEEDBACK_NOTIFY") or ""
    except Exception:
        v = os.environ.get("VOICEPI_FEEDBACK_NOTIFY") or ""
    return v.lower() not in ("", "0", "false", "no", "off")


def _play_windows(kind: str) -> None:
    """Play a beep via the Windows winsound module (lazy import)."""
    winsound = sys.modules.get("winsound")
    if winsound is None:
        import importlib
        winsound = importlib.import_module("winsound")
    if kind == "start":
        winsound.Beep(880, 80)
    else:
        winsound.Beep(440, 80)


# Freedesktop sound files used on Linux/PipeWire systems.
_FREEDESKTOP_START = "/usr/share/sounds/freedesktop/stereo/message.oga"
_FREEDESKTOP_STOP = "/usr/share/sounds/freedesktop/stereo/dialog-information.oga"


def _play_linux(kind: str) -> None:
    """Play a freedesktop sound file via paplay (non-blocking, best-effort)."""
    sound_file = _FREEDESKTOP_START if kind == "start" else _FREEDESKTOP_STOP
    if not os.path.exists(sound_file):
        return
    try:
        subprocess.Popen(
            ["paplay", sound_file],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def play_cue(kind: str) -> None:
    """Play a short audio cue indicating recording start or stop.

    Parameters
    ----------
    kind:
        ``"start"`` for a high-pitched beep / start sound;
        ``"stop"`` for a lower-pitched beep / stop sound.

    The function is a no-op when ``VOICEPI_FEEDBACK_SOUNDS`` is falsy or
    unset, and any exception is silently swallowed to keep the hot path
    free from any audio-subsystem fragility.
    """
    try:
        if not _sounds_enabled():
            return
        if os.name == "nt":
            _play_windows(kind)
        elif sys.platform == "linux":
            _play_linux(kind)
        # macOS / other platforms: no-op for now (document in CONFIGURATION.md)
    except Exception:
        pass


def notify_error(title: str, message: str) -> None:
    """Send a desktop notification for an error condition.

    Currently implemented on Linux via ``notify-send`` (Popen, non-blocking).
    Windows and macOS are no-ops for now — a future release may add
    ``win10toast`` / ``plyer`` / ``osascript`` support.

    The function is a no-op when ``VOICEPI_FEEDBACK_NOTIFY`` is falsy or
    unset, and any exception is silently swallowed.

    Parameters
    ----------
    title:
        Short notification heading, e.g. ``"whisper-dictate"``.
    message:
        Body text, e.g. ``"Model load failed: CUDA out of memory"``.
    """
    try:
        if not _notify_enabled():
            return
        if sys.platform == "linux":
            subprocess.Popen(
                ["notify-send", "--urgency=critical", title, message],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        # Windows / macOS: no-op (document in CONFIGURATION.md)
    except Exception:
        pass
