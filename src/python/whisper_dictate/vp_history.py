"""Local dictation history (JSONL) + the `history` CLI commands.

Local-only: accepted live dictations are appended via the Rust helper and can be
listed / copied / re-injected. Extracted from runtime.py.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from collections import deque
from pathlib import Path

from whisper_dictate.vp_config import get_value
from whisper_dictate.vp_rust import _rust_helper, _rust_json

_APPEND_RECORD_SINKS_SUPPORTED: bool | None = None


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _append_jsonl(path: str | None, event: dict) -> None:
    path = (path or "").strip()
    if not path:
        return
    _rust_json("append-jsonl", event, "--path", os.path.expanduser(path))


def _append_history(event: dict) -> None:
    path = event.get("_history_path")
    if path:
        _rust_json("append-history", event, "--path", str(path))
        return
    if history_enabled():
        _rust_json("append-history", event, "--path", str(history_path()))


def append_record_sinks(event: dict, *, metrics_jsonl: str | None, json_output: bool) -> None:
    global _APPEND_RECORD_SINKS_SUPPORTED
    raw_metrics_path = (metrics_jsonl or "").strip()
    metrics_path = os.path.expanduser(raw_metrics_path) if json_output and raw_metrics_path else ""
    history_target = event.get("_history_path")
    history_out = ""
    if history_target:
        history_out = str(history_target)
    elif history_enabled():
        history_out = str(history_path())
    if not metrics_path and not history_out:
        return
    payload = {
        "event": event,
        "metrics_path": metrics_path,
        "history_path": history_out,
    }
    if _APPEND_RECORD_SINKS_SUPPORTED is not False:
        if _rust_json("append-record-sinks", payload) is not None:
            _APPEND_RECORD_SINKS_SUPPORTED = True
            return
        _APPEND_RECORD_SINKS_SUPPORTED = False
    if metrics_path:
        _append_jsonl(metrics_path, event)
    if history_out:
        _rust_json("append-history", event, "--path", history_out)


def default_history_path() -> Path:
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "history.jsonl"
    return (
        Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state"))
        / "whisper-dictate"
        / "history.jsonl"
    )


def history_path() -> Path:
    raw = get_value("VOICEPI_HISTORY_JSONL")
    return Path(raw).expanduser() if raw else default_history_path()


def history_enabled() -> bool:
    return _truthy(get_value("VOICEPI_HISTORY_ENABLED", "1"))


def _history_event(event: dict) -> dict:
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


def append_history(event: dict, path: Path | None = None) -> Path | None:
    if not history_enabled():
        return None
    p = path or history_path()
    _rust_json("append-history", event, "--path", str(p))
    return p


def read_history(limit: int = 20, path: Path | None = None) -> list[dict]:
    p = path or history_path()
    if not p.exists():
        return []
    # Clamp to >=1 (matches the Rust `history list`); limit<=0 must not dump all.
    effective = max(1, limit)
    rows: deque[dict] = deque(maxlen=effective)
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
    return list(rows)


def last_history(path: Path | None = None) -> dict | None:
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


def _run_rust_history_command(*args: str) -> bool:
    helper = _rust_helper()
    if not helper:
        return False
    try:
        r = subprocess.run(
            [helper, "history", *args],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=5,
        )
    except Exception as e:
        print(f"[history] {e}", file=sys.stderr, flush=True)
        return False
    if r.returncode != 0:
        print((r.stderr or r.stdout).strip(), file=sys.stderr, flush=True)
        return False
    if r.stdout:
        print(r.stdout.rstrip("\n"), flush=True)
    return True


def _history_list(limit: int, as_json: bool) -> None:
    if as_json:
        rows = read_history(limit)
        print(json.dumps(rows, ensure_ascii=False, sort_keys=True), flush=True)
    elif not _run_rust_history_command("list", str(limit)):
        for row in read_history(limit):
            text = str(row.get("text", ""))
            ts = row.get("ts", "")
            backend = row.get("stt_backend", "")
            print(f"{ts} [{backend}] {text}", flush=True)


def _history_last(as_json: bool) -> None:
    if as_json:
        print(json.dumps(last_history() or {}, ensure_ascii=False, sort_keys=True), flush=True)
    elif not _run_rust_history_command("last"):
        print((last_history() or {}).get("text", ""), flush=True)


def run_history_command(action: str, *, limit: int = 10, as_json: bool = False) -> None:
    try:
        if action == "list":
            _history_list(limit, as_json)
        elif action == "last":
            _history_last(as_json)
        elif action == "copy-last":
            text = copy_last_to_clipboard()
            print(f"copied: {text}", flush=True)
        elif action == "reinject-last":
            text = reinject_last()
            print(f"re-injected: {text}", flush=True)
        else:
            raise RuntimeError(f"unknown history action: {action}")
    except Exception as e:
        print(f"[history] {e}", file=sys.stderr, flush=True)
        raise
