"""Optional Windows audio ducking while recording."""
from __future__ import annotations

import atexit
import os
import sys
from dataclasses import dataclass, field
from typing import Any

from vp_config import apply_config_to_environ, get_value

apply_config_to_environ()


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _float_setting(name: str, default: float, minimum: float, maximum: float) -> float:
    try:
        value = float(get_value(name, str(default)) or default)
    except (TypeError, ValueError):
        value = default
    return min(maximum, max(minimum, value))


@dataclass
class AudioDucker:
    enabled: bool
    target_volume: float
    _sessions: list[tuple[Any, float]] = field(default_factory=list)
    _warned: bool = False

    @classmethod
    def from_config(cls) -> "AudioDucker":
        return cls(
            enabled=_truthy(get_value("VOICEPI_AUDIO_DUCKING")),
            target_volume=_float_setting("VOICEPI_AUDIO_DUCKING_LEVEL", 0.25, 0.0, 1.0),
        )

    def enter(self) -> None:
        if not self.enabled or self._sessions:
            return
        if sys.platform != "win32":
            self._warn_once("audio ducking is only implemented on Windows")
            return
        try:
            import comtypes
            from pycaw.pycaw import AudioUtilities, ISimpleAudioVolume

            comtypes.CoInitialize()
            current_pid = os.getpid()
            for session in AudioUtilities.GetAllSessions():
                if session.Process and session.Process.pid == current_pid:
                    continue
                volume = session._ctl.QueryInterface(ISimpleAudioVolume)
                previous = float(volume.GetMasterVolume())
                if previous > self.target_volume:
                    volume.SetMasterVolume(self.target_volume, None)
                    self._sessions.append((volume, previous))
            if self._sessions:
                print(
                    f"[audio-duck] lowered {len(self._sessions)} audio sessions "
                    f"to {self.target_volume:.2f}",
                    flush=True,
                )
        except Exception as exc:  # noqa: BLE001 - optional feature must not block dictation
            self._sessions.clear()
            self._warn_once(f"audio ducking unavailable: {exc}")

    def exit(self) -> None:
        if not self._sessions:
            return
        restored = 0
        for volume, previous in reversed(self._sessions):
            try:
                volume.SetMasterVolume(previous, None)
                restored += 1
            except Exception:
                pass
        self._sessions.clear()
        print(f"[audio-duck] restored {restored} audio sessions", flush=True)

    def _warn_once(self, message: str) -> None:
        if self._warned:
            return
        self._warned = True
        print(f"[audio-duck] {message}", flush=True)


_ACTIVE_DUCKERS: list[AudioDucker] = []


def register_active_ducker(ducker: AudioDucker) -> AudioDucker:
    if ducker not in _ACTIVE_DUCKERS:
        _ACTIVE_DUCKERS.append(ducker)
    return ducker


def restore_all() -> None:
    for ducker in list(_ACTIVE_DUCKERS):
        ducker.exit()


atexit.register(restore_all)
