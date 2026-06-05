"""Local dictation history storage and helpers."""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

from whisper_dictate.vp_config import apply_config_to_environ, get_value

apply_config_to_environ()


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def default_history_path() -> Path:
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "history.jsonl"
    return Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "whisper-dictate" / "history.jsonl"


def history_path() -> Path:
    raw = get_value("VOICEPI_HISTORY_JSONL")
    return Path(raw).expanduser() if raw else default_history_path()


def history_enabled() -> bool:
    return _truthy(get_value("VOICEPI_HISTORY_ENABLED", "1"))


def _history_event(event: dict[str, Any]) -> dict[str, Any]:
    keys = (
        "ts", "event", "text", "raw_text", "text_preview", "text_chars",
        "dictionary_text",
        "recording_s", "audio_duration_s", "compute_s", "real_time_factor",
        "language", "language_probability", "model", "stt_backend", "device",
        "compute_type", "inject_mode", "inject_strategy", "target_title",
        "target_process", "profile", "dictionary_replacements",
        "post_processor", "post_mode", "post_model", "post_latency_ms",
        "post_changed", "post_fallback", "post_error",
    )
    return {key: event[key] for key in keys if key in event}


def append_history(event: dict[str, Any], path: Path | None = None) -> Path | None:
    if not history_enabled():
        return None
    p = path or history_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("a", encoding="utf-8") as f:
        json.dump(_history_event(event), f, ensure_ascii=False, sort_keys=True)
        f.write("\n")
    return p


def read_history(limit: int = 20, path: Path | None = None) -> list[dict[str, Any]]:
    p = path or history_path()
    if not p.exists():
        return []
    rows: list[dict[str, Any]] = []
    with p.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                rows.append(obj)
    return rows[-max(0, limit):] if limit else rows


def last_history(path: Path | None = None) -> dict[str, Any] | None:
    rows = read_history(1, path)
    return rows[-1] if rows else None


def copy_last_to_clipboard(path: Path | None = None) -> str:
    item = last_history(path)
    if not item or not item.get("text"):
        raise RuntimeError("history is empty")
    import pyperclip

    text = str(item["text"])
    pyperclip.copy(text)
    return text


def reinject_last(path: Path | None = None) -> str:
    text = copy_last_to_clipboard(path)
    from pynput import keyboard

    kb = keyboard.Controller()
    with kb.pressed(keyboard.Key.ctrl):
        kb.press("v")
        kb.release("v")
    return text


def print_history(limit: int = 10, *, as_json: bool = False) -> None:
    rows = read_history(limit)
    if as_json:
        print(json.dumps(rows, ensure_ascii=False, sort_keys=True), flush=True)
        return
    for row in rows:
        text = str(row.get("text", ""))
        ts = row.get("ts", "")
        backend = row.get("stt_backend", "")
        print(f"{ts} [{backend}] {text}", flush=True)


def run_history_command(action: str, *, limit: int = 10, as_json: bool = False) -> None:
    try:
        if action == "list":
            print_history(limit, as_json=as_json)
        elif action == "last":
            item = last_history()
            if as_json:
                print(json.dumps(item or {}, ensure_ascii=False, sort_keys=True),
                      flush=True)
            else:
                print((item or {}).get("text", ""), flush=True)
        elif action == "copy-last":
            text = copy_last_to_clipboard()
            print(f"copied: {text}", flush=True)
        elif action == "reinject-last":
            text = reinject_last()
            print(f"re-injected: {text}", flush=True)
        else:
            raise RuntimeError(f"unknown history action: {action}")
    except Exception as e:  # noqa: BLE001 - CLI helper should report cleanly
        print(f"[history] {e}", file=sys.stderr, flush=True)
        raise
