"""Structured metrics output for whisper-dictate."""
from __future__ import annotations

import json
import os
import sys
import time
from typing import Any


def truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def compact_text(text: str, limit: int = 240) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 3] + "..."


def base_event(**fields: Any) -> dict[str, Any]:
    event = {"ts": time.time()}
    event.update(fields)
    return event


def emit_json(event: dict[str, Any]) -> None:
    print(json.dumps(event, ensure_ascii=False, sort_keys=True), flush=True)


def append_jsonl(path: str | None, event: dict[str, Any]) -> None:
    if not path:
        return
    try:
        with open(os.path.expanduser(path), "a", encoding="utf-8") as f:
            json.dump(event, f, ensure_ascii=False, sort_keys=True)
            f.write("\n")
    except OSError as e:
        print(f"[metrics] could not write {path!r}: {e}", file=sys.stderr, flush=True)
