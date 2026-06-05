"""Deterministic spoken formatting commands."""
from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass, field

from whisper_dictate.vp_config import get_value


@dataclass(frozen=True)
class FormatCommandResult:
    text: str
    enabled: bool
    changed: bool = False
    command_set: str = "off"
    applied: list[dict[str, str]] = field(default_factory=list)


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _normalize_command_set(raw: str | None) -> str:
    raw = (raw or "off").strip().lower()
    if _truthy(raw) and raw not in ("en", "da", "both", "all"):
        return "both"
    if raw == "all":
        return "both"
    if raw in ("en", "da", "both"):
        return raw
    return "off"


def _command_set() -> str:
    return _normalize_command_set(get_value("VOICEPI_FORMAT_COMMANDS", "off"))


def apply_format_commands(text: str, command_set: str | None = None) -> FormatCommandResult:
    selected = _normalize_command_set(command_set) if command_set is not None else _command_set()
    if selected == "off":
        return FormatCommandResult(text=text, enabled=False, command_set="off")
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        raise RuntimeError("Rust format-text helper is not available")
    try:
        r = subprocess.run(
            [
                helper,
                "format-text",
                "--text",
                text,
                "--command-set",
                selected,
            ],
            capture_output=True,
            timeout=5,
            text=True,
        )
        if r.returncode != 0:
            err = (r.stderr or "").strip()
            raise RuntimeError(err or "Rust format-text helper failed")
        payload = json.loads(r.stdout)
        return FormatCommandResult(
            text=str(payload.get("text", text)),
            enabled=bool(payload.get("enabled", False)),
            changed=bool(payload.get("changed", False)),
            command_set=str(payload.get("command_set", "off")),
            applied=[
                {
                    "command": str(item.get("command", "")),
                    "replacement": str(item.get("replacement", "")),
                    "count": str(item.get("count", "0")),
                }
                for item in payload.get("applied", [])
                if isinstance(item, dict)
            ],
        )
    except json.JSONDecodeError as e:
        raise RuntimeError("Rust format-text helper returned invalid JSON") from e
