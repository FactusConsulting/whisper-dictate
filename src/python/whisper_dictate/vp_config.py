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


SETTINGS: tuple[Setting, ...] = (
    Setting("VOICEPI_KEY", "key", "ctrl_r", live=False),
    Setting("VOICEPI_MODEL", "model", "large-v3-turbo", live=False),
    Setting("VOICEPI_STT_BACKEND", "stt_backend", "whisper", live=False),
    Setting("VOICEPI_STT_MODEL", "stt_model", None, live=False),
    Setting("VOICEPI_STT_BASE_URL", "stt_base_url", "https://api.openai.com/v1", live=False),
    Setting("VOICEPI_STT_TIMEOUT_MS", "stt_timeout_ms", "30000", live=False),
    Setting("VOICEPI_PARAKEET_MODEL", "parakeet_model", None, live=False),
    Setting("VOICEPI_DEVICE", "device", "auto", live=False),
    Setting("VOICEPI_COMPUTE_TYPE", "compute_type", None, live=False),
    Setting("VOICEPI_LANG", "lang", None, live=True),
    Setting("VOICEPI_XKB_LAYOUT", "xkb_layout", None, live=True),
    Setting("VOICEPI_INITIAL_PROMPT", "initial_prompt", None, live=True),
    Setting("VOICEPI_INJECT_MODE", "inject_mode", "auto", live=True),
    Setting("VOICEPI_FORMAT_COMMANDS", "format_commands", "off", live=True),
    Setting("VOICEPI_BEAM_SIZE", "beam_size", "1", live=True),
    Setting("VOICEPI_TEMPERATURE", "temperature", "0.0,0.2", live=True),
    Setting("VOICEPI_CONTEXT_MIN_SECONDS", "context_min_seconds", "5", live=True),
    Setting("VOICEPI_PARAKEET_MIN_SECONDS", "parakeet_min_seconds", "1.5", live=True),
    Setting("VOICEPI_RELEASE_TAIL_MS", "release_tail_ms", "200", live=True),
    Setting("VOICEPI_VAD_THRESHOLD", "vad_threshold", "0.3", live=True),
    Setting("VOICEPI_VAD_MIN_SILENCE_MS", "vad_min_silence_ms", "600", live=True),
    Setting("VOICEPI_VAD_SPEECH_PAD_MS", "vad_speech_pad_ms", "200", live=True),
    Setting("VOICEPI_TARGET_DBFS", "target_dbfs", "-20", live=True),
    Setting("VOICEPI_MIN_INPUT_DBFS", "min_input_dbfs", "-55", live=True),
    Setting("VOICEPI_MIN_SNR_DB", "min_snr_db", "6", live=True),
    Setting("VOICEPI_AUDIO_DUCKING", "audio_ducking", None, live=True),
    Setting("VOICEPI_AUDIO_DUCKING_LEVEL", "audio_ducking_level", "0.25", live=True),
    Setting("VOICEPI_DICTIONARY", "dictionary", None, live=True),
    Setting("VOICEPI_DICTIONARY_ENABLED", "dictionary_enabled", "1", live=True),
    Setting("VOICEPI_DICTIONARY_MAX_TERMS", "dictionary_max_terms", "80", live=True),
    Setting("VOICEPI_DICTIONARY_PROMPT_CHARS", "dictionary_prompt_chars", "1200", live=True),
    Setting("VOICEPI_JSON", "json_output", None, live=True),
    Setting("VOICEPI_METRICS_JSONL", "metrics_jsonl", None, live=True),
    Setting("VOICEPI_COMMAND_HOOK", "command_hook", None, live=True),
    Setting("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", "command_hook_timeout_ms", "2000", live=True),
    Setting("VOICEPI_HISTORY_ENABLED", "history_enabled", "1", live=True),
    Setting("VOICEPI_HISTORY_JSONL", "history_jsonl", None, live=True),
    Setting("VOICEPI_LOCAL_ONLY", "local_only", None, live=False),
    Setting("VOICEPI_POST_PROCESSOR", "post_processor", "none", live=True),
    Setting("VOICEPI_POST_MODE", "post_mode", "raw", live=True),
    Setting("VOICEPI_POST_MODEL", "post_model", "qwen2.5:3b", live=True),
    Setting("VOICEPI_POST_BASE_URL", "post_base_url", "http://localhost:11434", live=True),
    Setting("VOICEPI_POST_TIMEOUT_MS", "post_timeout_ms", "2000", live=True),
    Setting("VOICEPI_POST_MAX_INPUT_CHARS", "post_max_input_chars", "4000", live=True),
    Setting("VOICEPI_POST_MAX_OUTPUT_CHARS", "post_max_output_chars", "4000", live=True),
    Setting("VOICEPI_POST_REDACT", "post_redact", None, live=True),
    Setting("VOICEPI_POST_REDACT_TERMS", "post_redact_terms", None, live=True),
    Setting("VOICEPI_DEBUG", "debug", None, live=True),
    Setting("VOICEPI_STT_DEBUG", "stt_debug", None, live=True),
    Setting("VOICEPI_QUIT_KEY", "quit_key", "esc", live=False),
    Setting("VOICEPI_QUIT_COUNT", "quit_count", "3", live=False),
    Setting("VOICEPI_QUIT_WINDOW_MS", "quit_window_ms", "1500", live=False),
)

SETTING_BY_ENV = {s.env: s for s in SETTINGS}
SETTING_BY_KEY = {s.key: s for s in SETTINGS}


def config_path() -> Path:
    raw = os.environ.get(CONFIG_ENV)
    if raw:
        return Path(raw).expanduser()
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "config.json"
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "whisper-dictate" / "config.json"


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


def save_config(data: dict[str, Any], path: Path | None = None) -> Path:
    path = path or config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    cleaned = {k: v for k, v in data.items() if v not in (None, "")}
    path.write_text(json.dumps(cleaned, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return path


def get_value(env: str, default: str | None = None) -> str | None:
    setting = SETTING_BY_ENV.get(env)
    if setting:
        data = load_config()
        value = data.get(setting.key)
        if value not in (None, ""):
            return str(value)
    value = os.environ.get(env)
    if value not in (None, ""):
        return value
    if setting and setting.default is not None:
        return setting.default
    return default


def apply_config_to_environ() -> set[str]:
    """Overlay configured JSON settings into os.environ.

    Existing env vars remain the fallback when a key is absent from config.json.
    """
    data = load_config()
    changed: set[str] = set()
    for key, value in data.items():
        setting = SETTING_BY_KEY.get(key)
        if not setting:
            continue
        new_value = "" if value is None else str(value)
        if os.environ.get(setting.env) != new_value:
            os.environ[setting.env] = new_value
            changed.add(setting.env)
    return changed


def effective_config() -> dict[str, str]:
    data = load_config()
    out: dict[str, str] = {}
    for setting in SETTINGS:
        value = data.get(setting.key)
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
