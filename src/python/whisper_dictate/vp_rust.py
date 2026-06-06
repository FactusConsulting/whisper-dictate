"""Shared Python-to-Rust helper boundary."""
from __future__ import annotations

import json
import os
import subprocess


def helper_path() -> str | None:
    return os.environ.get("VOICEPI_RUST_INJECTOR") or None


def run_json_helper(
    command: str,
    payload: dict[str, object],
    *args: str,
    timeout: float = 5.0,
) -> dict[str, object] | None:
    helper = helper_path()
    if not helper:
        return None
    try:
        result = subprocess.run(
            [helper, command, *args],
            input=json.dumps(payload, ensure_ascii=False, sort_keys=True),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
            shell=False,
        )
        if result.returncode != 0:
            return None
        decoded = json.loads(result.stdout or "{}")
        return decoded if isinstance(decoded, dict) else None
    except Exception:
        return None
