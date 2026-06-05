"""Structured worker events for the Rust app/controller.

Human-readable stdout stays the default CLI surface. When the Rust controller
sets VOICEPI_WORKER_EVENTS=1, selected runtime status changes are also emitted
as compact JSON lines on stderr so the controller can track state without
scraping localized/free-form log text.
"""
from __future__ import annotations

import json
import os
import sys
from typing import Any


EVENT_ENV = "VOICEPI_WORKER_EVENTS"
EVENT_PREFIX = "[worker-event] "


def enabled() -> bool:
    return (os.environ.get(EVENT_ENV) or "").strip().lower() in (
        "1", "true", "yes", "on")


def emit(event: str, **fields: Any) -> None:
    if not enabled():
        return
    payload = {"event": event}
    payload.update({k: v for k, v in fields.items() if v is not None})
    print(
        EVENT_PREFIX + json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
        file=sys.stderr,
        flush=True,
    )
