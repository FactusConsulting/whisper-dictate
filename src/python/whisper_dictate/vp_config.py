"""Persistent user configuration for whisper-dictate.

The app still honours VOICEPI_* environment variables, but a JSON config file
is easier for a UI to edit and can be reloaded while the dictation process is
running.
"""
from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


CONFIG_ENV = "VOICEPI_CONFIG"


@dataclass(frozen=True)
class Setting:
    env: str
    key: str
    default: str | None = None
    live: bool = True


@dataclass(frozen=True)
class ConfigSnapshot:
    data: dict[str, Any]

    def get_value(self, env: str, default: str | None = None) -> str | None:
        setting = SETTING_BY_ENV.get(env)
        if setting:
            value = self.data.get(setting.key)
            if value not in (None, ""):
                return str(value)
        value = os.environ.get(env)
        if value not in (None, ""):
            return value
        if setting and setting.default is not None:
            return setting.default
        return default

    def effective_config(self) -> dict[str, str]:
        out: dict[str, str] = {}
        for setting in SETTINGS:
            value = self.data.get(setting.key)
            if value not in (None, ""):
                out[setting.key] = str(value)
                continue
            env_value = os.environ.get(setting.env)
            if env_value not in (None, ""):
                out[setting.key] = str(env_value)
                continue
            if setting.default is not None:
                out[setting.key] = setting.default
        return out


def _load_settings_schema() -> tuple[Setting, ...]:
    """Load the canonical runtime-settings schema.

    ``settings_schema.json`` (next to this module) is the SINGLE SOURCE OF TRUTH
    for the VOICEPI_* env var <-> config key <-> default <-> live mapping. The
    Rust controller embeds the same file via ``include_str!``. Add or change
    settings there, not in a hand-maintained table here.
    """
    path = Path(__file__).with_name("settings_schema.json")
    data = json.loads(path.read_text(encoding="utf-8"))
    return tuple(
        Setting(
            env=row["env"],
            key=row["key"],
            default=row.get("default"),
            live=bool(row.get("live", True)),
        )
        for row in data["settings"]
    )


SETTINGS: tuple[Setting, ...] = _load_settings_schema()

SETTING_BY_ENV = {s.env: s for s in SETTINGS}
SETTING_BY_KEY = {s.key: s for s in SETTINGS}


def appdata_dir() -> Path:
    """User-managed config directory holding ``config.json`` / ``dictionary.json``.

    On Windows this is ``%APPDATA%\\WhisperDictate``; on Linux/macOS it is
    ``$XDG_CONFIG_HOME/whisper-dictate`` (defaulting to ``~/.config``). This is
    the single helper for the per-user dir that survives reinstalls — other
    features (e.g. the benchmark corpus/audio fallback) join into it so users
    keep one stable location for everything they manage by hand.
    """
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate"
    return Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config")) / "whisper-dictate"


def config_path() -> Path:
    raw = os.environ.get(CONFIG_ENV)
    if raw:
        return Path(raw).expanduser()
    return appdata_dir() / "config.json"


def load_config(path: Path | None = None) -> dict[str, Any]:
    path = path or config_path()
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception as e:  # noqa: BLE001 - config should not prevent startup
        print(f"[config] could not load {path}: {e}", flush=True)
        return {}
    if not isinstance(data, dict):
        print(f"[config] ignoring {path}: root must be an object", flush=True)
        return {}
    return data


def config_snapshot(path: Path | None = None) -> ConfigSnapshot:
    return ConfigSnapshot(load_config(path))


def save_config(data: dict[str, Any], path: Path | None = None) -> Path:
    path = path or config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    cleaned = {k: v for k, v in data.items() if v not in (None, "")}
    path.write_text(json.dumps(cleaned, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return path


def get_value(env: str, default: str | None = None) -> str | None:
    if env not in SETTING_BY_ENV:
        value = os.environ.get(env)
        return value if value not in (None, "") else default
    return config_snapshot().get_value(env, default)


def apply_config_to_environ() -> set[str]:
    """Overlay configured JSON settings into os.environ.

    Existing env vars remain the fallback when a key is absent from config.json.

    Diagnostic note: when VOICEPI_AUDIO_DEVICE is among the updated env vars,
    a one-line trace is emitted to stderr (the runtime supervisor mirrors it
    into the UI log). The audio-device-switch-stale bug surfaces here: if you
    SAVE a new mic in the UI but this line does NOT appear, the new value
    never reached env (which means the config-file load saw stale or empty
    data), and the next capture-open will use whatever env had before.
    """
    import sys
    data = load_config()
    changed: set[str] = set()
    audio_device_before = os.environ.get("VOICEPI_AUDIO_DEVICE", "")
    for key, value in data.items():
        setting = SETTING_BY_KEY.get(key)
        if not setting:
            continue
        new_value = "" if value is None else str(value)
        if os.environ.get(setting.env) != new_value:
            os.environ[setting.env] = new_value
            changed.add(setting.env)
    if "VOICEPI_AUDIO_DEVICE" in changed:
        print(
            f"[config] VOICEPI_AUDIO_DEVICE: {audio_device_before!r} → "
            f"{os.environ.get('VOICEPI_AUDIO_DEVICE', '')!r} "
            f"(config has audio_device={data.get('audio_device')!r})",
            file=sys.stderr,
            flush=True,
        )
    elif "audio_device" in data and data["audio_device"] != audio_device_before:
        # Config has a different audio_device value but env wasn't updated
        # — should never happen, log it loudly if it does.
        print(
            f"[config] WARN: config has audio_device={data['audio_device']!r} "
            f"but VOICEPI_AUDIO_DEVICE stays {audio_device_before!r}",
            file=sys.stderr,
            flush=True,
        )
    return changed


def effective_config() -> dict[str, str]:
    return config_snapshot().effective_config()


def config_mtime(path: Path | None = None) -> float:
    path = path or config_path()
    reload_path = path.with_suffix(".reload")
    stamps = []
    try:
        stamps.append(path.stat().st_mtime)
    except OSError:
        pass
    try:
        stamps.append(reload_path.stat().st_mtime)
    except OSError:
        pass
    return max(stamps) if stamps else 0.0


def touch_reload_signal() -> Path:
    path = config_path().with_suffix(".reload")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(str(time.time()), encoding="utf-8")
    return path
