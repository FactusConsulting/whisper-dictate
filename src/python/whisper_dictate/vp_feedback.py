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
* Cues are **non-blocking**: subprocess cues use ``Popen`` (reaped by a
  detached waiter thread so no zombies accumulate) and the Windows beep runs
  on its own daemon thread because ``winsound.Beep`` is synchronous and
  ``play_cue("start")`` is called on the key-handler thread.
* Settings are read **live from os.environ** on each call — zero disk I/O on
  the hot path. That is sufficient for live reload because the worker overlays
  config.json onto the environment at startup AND on every live config reload
  (``vp_config.apply_config_to_environ`` via
  ``Dictate._reload_live_config_if_changed``).
"""
from __future__ import annotations

import os
import subprocess
import sys
import threading

# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _env_truthy(name: str) -> bool:
    """Live, disk-free settings gate: config.json is already overlaid onto the
    environment (startup + every live reload), so the env var IS the setting."""
    value = (os.environ.get(name) or "").strip()
    return value.lower() not in ("", "0", "false", "no", "off")


def _sounds_enabled() -> bool:
    return _env_truthy("VOICEPI_FEEDBACK_SOUNDS")


def _notify_enabled() -> bool:
    return _env_truthy("VOICEPI_FEEDBACK_NOTIFY")


def _reap(proc: "subprocess.Popen") -> None:
    """Wait out a fire-and-forget subprocess on a daemon thread.

    Dropping the Popen handle without waiting leaks zombie processes on POSIX;
    a detached waiter keeps the cue/notify call itself non-blocking.
    """
    threading.Thread(target=proc.wait, daemon=True).start()


def _play_windows(kind: str) -> None:
    """Play a beep via winsound — on a daemon thread, because ``winsound.Beep``
    is SYNCHRONOUS (blocks ~80 ms) and the start cue runs on the key-handler
    thread."""
    winsound = sys.modules.get("winsound")
    if winsound is None:
        import importlib
        winsound = importlib.import_module("winsound")
    frequency = 880 if kind == "start" else 440

    def _beep() -> None:
        try:
            winsound.Beep(frequency, 80)
        except Exception:
            pass

    threading.Thread(target=_beep, daemon=True).start()


# Freedesktop sound files used on Linux/PipeWire systems.
_FREEDESKTOP_START = "/usr/share/sounds/freedesktop/stereo/message.oga"
_FREEDESKTOP_STOP = "/usr/share/sounds/freedesktop/stereo/dialog-information.oga"

# Tried in order; first found on PATH wins.
_LINUX_PLAYERS = ["paplay", "pw-play"]


def _play_linux(kind: str) -> None:
    """Play a freedesktop sound file via paplay or pw-play (non-blocking, best-effort)."""
    sound_file = _FREEDESKTOP_START if kind == "start" else _FREEDESKTOP_STOP
    if not os.path.exists(sound_file):
        return
    for player in _LINUX_PLAYERS:
        try:
            _reap(subprocess.Popen(
                [player, sound_file],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            ))
            return
        except FileNotFoundError:
            continue
        except Exception:
            return


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

    Currently implemented on Linux via ``notify-send`` (Popen, non-blocking,
    reaped on a daemon thread). Windows and macOS are no-ops for now — a
    future release may add ``win10toast`` / ``plyer`` / ``osascript`` support.

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
            _reap(subprocess.Popen(
                ["notify-send", "--urgency=critical", title, message],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            ))
        # Windows / macOS: no-op (document in CONFIGURATION.md)
    except Exception:
        pass
