"""Optional command hook for structured utterance events."""
from __future__ import annotations

import json
import os
import shlex
import subprocess
import time
from dataclasses import dataclass
from typing import Any

from vp_config import get_value


@dataclass(frozen=True)
class CommandHookResult:
    enabled: bool = False
    command: str = ""
    returncode: int | None = None
    latency_ms: int = 0
    timeout: bool = False
    error: str | None = None


def _parse_command(command: str) -> list[str]:
    command = command.strip()
    if not command:
        return []
    if command.startswith("["):
        parsed = json.loads(command)
        if not isinstance(parsed, list) or not all(isinstance(p, str) for p in parsed):
            raise ValueError("VOICEPI_COMMAND_HOOK JSON form must be an array of strings")
        return parsed
    return shlex.split(command, posix=(os.name != "nt"))


def run_command_hook(event: dict[str, Any]) -> CommandHookResult:
    command = get_value("VOICEPI_COMMAND_HOOK") or ""
    if not command.strip():
        return CommandHookResult()
    timeout_ms = int(float(get_value("VOICEPI_COMMAND_HOOK_TIMEOUT_MS", "2000") or "2000"))
    timeout_s = max(1, timeout_ms) / 1000.0
    t0 = time.monotonic()
    try:
        argv = _parse_command(command)
        if not argv:
            return CommandHookResult()
        proc = subprocess.run(
            argv,
            input=json.dumps(event, ensure_ascii=False, sort_keys=True),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s,
            shell=False,
        )
        return CommandHookResult(
            enabled=True,
            command=command,
            returncode=proc.returncode,
            latency_ms=int((time.monotonic() - t0) * 1000),
            error=(proc.stderr or "").strip()[-1000:] or None,
        )
    except subprocess.TimeoutExpired:
        return CommandHookResult(
            enabled=True,
            command=command,
            latency_ms=int((time.monotonic() - t0) * 1000),
            timeout=True,
            error=f"command hook timed out after {timeout_ms}ms",
        )
    except Exception as exc:  # noqa: BLE001 - hook failures must not break dictation
        return CommandHookResult(
            enabled=True,
            command=command,
            latency_ms=int((time.monotonic() - t0) * 1000),
            error=str(exc),
        )


def annotate_event_with_hook(event: dict[str, Any], result: CommandHookResult) -> None:
    event.update({
        "command_hook_enabled": result.enabled,
        "command_hook_command": result.command or None,
        "command_hook_returncode": result.returncode,
        "command_hook_latency_ms": result.latency_ms,
        "command_hook_timeout": result.timeout,
        "command_hook_error": result.error,
    })
