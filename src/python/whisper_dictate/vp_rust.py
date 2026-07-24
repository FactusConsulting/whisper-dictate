"""Shared Python-to-Rust helper boundary."""
from __future__ import annotations

import json
import os
import subprocess
import sys


def helper_path() -> str | None:
    return os.environ.get("VOICEPI_RUST_INJECTOR") or None


def no_console_window_kwargs() -> dict[str, object]:
    """Return subprocess kwargs that suppress a Windows cmd flash.

    The Rust CLI helper (`whisper-dictate.exe`) is a console-subsystem binary
    since the two-binary split (PR #564) — spawning it with a plain
    ``subprocess.run([...])`` from the tray-launched worker allocates a fresh
    console window, which flashes on screen every time. On non-Windows the
    concept is a no-op; return an empty dict so ``**no_console_window_kwargs()``
    is safe to unpack unconditionally at any call site.
    """
    if os.name != "nt":
        return {}
    # subprocess.CREATE_NO_WINDOW exists on the Windows ``subprocess`` module
    # only; guard via getattr so the import doesn't ImportError on Linux CI.
    flag = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    return {"creationflags": flag}


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
            **no_console_window_kwargs(),
        )
        if result.returncode != 0:
            return None
        decoded = json.loads(result.stdout or "{}")
        return decoded if isinstance(decoded, dict) else None
    except Exception:
        return None


# Verbose worker-side bridge (logs helper stderr). Distinct from the quiet
# run_json_helper above; TODO: reconcile into one helper.
def _rust_helper() -> str | None:
    return helper_path()


def _rust_json(command: str, payload: dict, *args: str, timeout: float = 5.0) -> dict | None:
    helper = _rust_helper()
    if not helper:
        return None
    try:
        r = subprocess.run(
            [helper, command, *args],
            input=json.dumps(payload, ensure_ascii=False, sort_keys=True),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout,
            shell=False,
            **no_console_window_kwargs(),
        )
        if r.returncode != 0:
            err = (r.stderr or "").strip()
            if err:
                print(f"[rust:{command}] {err}", file=sys.stderr, flush=True)
            return None
        if r.stderr:
            print(r.stderr, end="", file=sys.stderr, flush=True)
        if not (r.stdout or "").strip():
            return {}
        result = json.loads(r.stdout)
        return result if isinstance(result, dict) else None
    except Exception as e:  # noqa: BLE001 - helper failures should not stop dictation
        print(f"[rust:{command}] {e}", file=sys.stderr, flush=True)
        return None
