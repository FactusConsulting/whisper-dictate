"""Dispatch between the Python ``Dictate(...).run()`` loop and the Rust
``whisper-dictate dictate-run`` subprocess.

Audit item 5 Phase A step 2 (see
``docs/design/item5-wire-dictate-session.md``). Reads the env var
``VOICEPI_DICTATE_ENGINE`` and, when set to ``rust``, subprocesses the
Rust CLI verb (added in PR #512) instead of running the in-process
Python PTT loop.

Contract:

* Default (unset or empty) is ``python`` — byte-identical to the pre-item-5
  behaviour. Nobody who does not opt in sees a difference.
* ``rust`` opts into the subprocess path. The Rust child emits JSONL
  events on stdout; :func:`run_rust_engine` forwards each line verbatim
  to the Python parent's stdout so the Rust supervisor above sees the
  same event stream it would have seen from the Python worker.
* If the Rust engine cannot start (binary missing, features not
  compiled in, spawn error, or an early exit before the READY signal),
  we log the reason and return ``(False, code)`` so the caller falls
  back to the Python engine. Rust is opt-in — a broken opt-in must NOT
  take down the whole worker.
* Any other value logs a warning and falls back to Python.

The module deliberately keeps every subprocess seam explicit
(``_spawn`` / ``_stdout_sink`` kwargs on :func:`run_rust_engine`) so the
tests can drive the dispatch without a real Rust binary.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from typing import Callable, Optional


# --- public constants ---------------------------------------------------

ENGINE_ENV = "VOICEPI_DICTATE_ENGINE"
ENGINE_PYTHON = "python"
ENGINE_RUST = "rust"

# The Rust `dictate-run --json-events` verb prints this envelope BEFORE
# entering its event loop. Matching all three fields pins the machine
# contract so a future rename in the Rust side surfaces here as a test
# failure rather than a silent forward-without-ready.
_READY_KIND = "ready"
_READY_ENGINE = "rust"


# --- engine selection ---------------------------------------------------


def select_engine(env: Optional[dict] = None) -> str:
    """Return the canonical engine name (``"python"`` or ``"rust"``).

    An unset or empty env var resolves to ``"python"``. An unknown value
    also resolves to ``"python"`` — call :func:`is_known_engine` on the
    raw value first if you want to log a warning for that case.
    """
    if env is None:
        env = os.environ
    raw = _raw_engine(env)
    if raw in (ENGINE_PYTHON, ENGINE_RUST):
        return raw
    return ENGINE_PYTHON


def is_known_engine(name: Optional[str]) -> bool:
    """True iff the given raw string names an engine we recognise.

    Empty string / ``None`` count as "not set" (also True — the default
    is well-defined), so the caller can distinguish the unknown case
    with ``is_known_engine(raw)``.
    """
    if name is None:
        return True
    lowered = name.strip().lower()
    return lowered in ("", ENGINE_PYTHON, ENGINE_RUST)


def _raw_engine(env: dict) -> str:
    return (env.get(ENGINE_ENV) or "").strip().lower()


# --- Rust subprocess dispatch -------------------------------------------


def resolve_whisper_dictate_binary() -> Optional[str]:
    """Find the ``whisper-dictate`` binary the Rust engine needs.

    Prefers ``VOICEPI_RUST_INJECTOR`` (set by the Rust supervisor when it
    spawns this Python worker — see
    ``src/rust/runtime/worker_command.rs``), then falls back to
    ``shutil.which``.
    """
    hint = os.environ.get("VOICEPI_RUST_INJECTOR")
    if hint:
        return hint
    return shutil.which("whisper-dictate")


def _build_dictate_run_args(
    binary: str,
    config_path: Optional[str],
    json_events: bool = True,
) -> list:
    args = [binary, "dictate-run"]
    if json_events:
        args.append("--json-events")
    if config_path:
        args.extend(["--config", config_path])
    return args


def _log(message: str) -> None:
    """Structured stderr log. Uses the ``[runtime]`` prefix so the Rust
    supervisor's log tail groups these with the rest of the worker's
    setup lines."""
    print(f"[runtime] {message}", file=sys.stderr, flush=True)


def _default_spawn(args: list) -> subprocess.Popen:
    return subprocess.Popen(  # noqa: S603 — args are a fixed exec list
        args,
        stdout=subprocess.PIPE,
        stderr=None,  # inherit so Rust warnings/errors reach the supervisor
        stdin=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,  # line-buffered so ready-signal arrives promptly
    )


def _forward_line(line: str) -> None:
    """Default sink: forward the JSON line to this process's stdout so
    the Rust supervisor above receives the Rust engine's events on the
    same channel it reads Python worker events from."""
    print(line, flush=True)


def _is_ready_event(line: str) -> bool:
    try:
        payload = json.loads(line)
    except (json.JSONDecodeError, ValueError):
        return False
    if not isinstance(payload, dict):
        return False
    return (
        payload.get("kind") == _READY_KIND
        and payload.get("ready") is True
        and payload.get("engine") == _READY_ENGINE
    )


def run_rust_engine(
    config_path: Optional[str] = None,
    *,
    _spawn: Optional[Callable] = None,
    _stdout_sink: Optional[Callable] = None,
) -> tuple:
    """Run the Rust dictation engine as a subprocess.

    Returns ``(ran, code)``:

    * ``(False, None)`` — could not locate or spawn the binary. The
      caller MUST fall back to the Python engine.
    * ``(False, code)`` — subprocess started but exited before emitting
      the READY signal (feature-flag error, immediate crash, …). The
      caller SHOULD fall back to the Python engine and surface ``code``.
    * ``(True, code)`` — subprocess emitted READY and then exited with
      ``code``. The caller propagates ``code`` to its own exit path.

    ``_spawn`` / ``_stdout_sink`` are test seams — production code
    should call this with no kwargs.
    """
    binary = resolve_whisper_dictate_binary()
    if not binary:
        _log(
            "dictate engine=rust: `whisper-dictate` binary not found "
            "(set VOICEPI_RUST_INJECTOR or add it to PATH); "
            "falling back to python engine"
        )
        return (False, None)

    args = _build_dictate_run_args(binary, config_path, json_events=True)
    spawn = _spawn or _default_spawn
    sink = _stdout_sink or _forward_line

    _log(
        "dictate engine=rust: subprocessing whisper-dictate dictate-run "
        f"(binary={binary!r}, config={config_path!r})"
    )

    try:
        proc = spawn(args)
    except (OSError, ValueError) as exc:
        _log(
            f"dictate engine=rust: subprocess spawn failed ({exc!r}); "
            "falling back to python engine"
        )
        return (False, None)

    seen_ready = False
    try:
        stdout = getattr(proc, "stdout", None)
        if stdout is not None:
            for raw_line in stdout:
                line = raw_line.rstrip("\r\n")
                if not line:
                    continue
                if not seen_ready and _is_ready_event(line):
                    seen_ready = True
                    _log(
                        "dictate engine=rust: ready-signal received; "
                        "forwarding events to supervisor"
                    )
                sink(line)
    except KeyboardInterrupt:
        # Propagate Ctrl-C to the child and let the finally block reap it.
        try:
            proc.terminate()
        except OSError:
            pass
    finally:
        try:
            code = proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            code = proc.wait()
        except Exception:  # noqa: BLE001 — wait() should not mask the outer flow
            code = None

    if not seen_ready:
        _log(
            f"dictate engine=rust: subprocess exited without READY "
            f"signal (code={code}); falling back to python engine"
        )
        return (False, code)

    _log(f"dictate engine=rust: subprocess exited (code={code})")
    return (True, code)
