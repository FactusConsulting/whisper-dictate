"""Wave 5 shell-out shim for the dictation orchestrator pure helpers.

Mirrors the Wave 4-A ``vp_dictionary_training._rust_dictionary_ops`` pattern:
the Python orchestrator stays the caller-facing implementation, but small
pure-logic decisions (``Dictate._should_skip_pcm``, the restart-required
key diff, STT-backend validation + human-label resolution, env-flag
parsing) can opt into the Rust port by setting ``VOICEPI_DICTATE_BACKEND=
rust`` + a ``VOICEPI_RUST_INJECTOR`` pointing at the ``whisper-dictate``
binary. Default install leaves the gate off, so the shipping behaviour is
byte-identical to today.

The shell-out is intended for startup-time / once-per-recording-start
queries — NOT the per-utterance transcription hot path (the subprocess
round-trip would add tens of milliseconds to every recording). When the
Rust supervisor (Wave 8) takes over the full event loop, the in-process
Rust calls drop the JSON envelope entirely.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from typing import Any

_ENV_BACKEND = "VOICEPI_DICTATE_BACKEND"
_ENV_HELPER = "VOICEPI_RUST_INJECTOR"
_HELPER_SUBCOMMAND = "dictate-ops"
_DEFAULT_TIMEOUT_S = 5.0


def rust_backend_enabled() -> bool:
    """True iff the user opted in via ``VOICEPI_DICTATE_BACKEND=rust``.

    Pulled out so callers can short-circuit without building the params
    payload — the gate is off by default and the helper subprocess is
    relatively expensive compared to the in-process Python decision.
    """
    return (os.environ.get(_ENV_BACKEND) or "").strip().lower() == "rust"


def _resolve_helper() -> str | None:
    """Return the binary path to shell out to, or ``None`` if unavailable.

    Same env var (``VOICEPI_RUST_INJECTOR``) every other Rust shell-out
    uses; keeps the operator-facing config story uniform across waves.
    """
    helper = os.environ.get(_ENV_HELPER) or ""
    return helper or None


def call_op(op: str, params: dict[str, Any], *, timeout_s: float = _DEFAULT_TIMEOUT_S) -> Any | None:
    """Shell out to ``whisper-dictate dictate-ops`` with ``{op, params}``.

    Returns the parsed JSON response on success, or ``None`` on ANY failure
    (backend off, helper missing, helper exited non-zero, malformed JSON).
    The caller then falls back to the in-process Python implementation so
    a misconfigured opt-in never regresses behaviour. Any helper error is
    logged to stderr (consistent with the other ``[rust:*-ops]`` shims) so
    a debugging operator can see why the fallback fired.
    """
    if not rust_backend_enabled():
        return None
    helper = _resolve_helper()
    if helper is None:
        return None
    payload = {"op": op, "params": params}
    try:
        result = subprocess.run(
            [helper, _HELPER_SUBCOMMAND],
            input=json.dumps(payload, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=timeout_s,
            shell=False,
        )
    except Exception as exc:  # noqa: BLE001 — never crash the worker on a helper hiccup
        print(f"[rust:dictate-ops] {exc}", file=sys.stderr, flush=True)
        return None
    if result.returncode != 0:
        err = (result.stderr or "").strip()
        if err:
            print(f"[rust:dictate-ops] {err}", file=sys.stderr, flush=True)
        return None
    try:
        return json.loads(result.stdout or "{}")
    except Exception as exc:  # noqa: BLE001 — bad JSON is a helper bug, fall back to Python
        print(f"[rust:dictate-ops] invalid JSON: {exc}", file=sys.stderr, flush=True)
        return None


# ---------------------------------------------------------------- typed wrappers

def rust_validate_backend(backend: str) -> tuple[str, str] | None:
    """Validate a backend identifier via the Rust helper.

    Returns ``(canonical_backend, human_label)`` on success, ``None`` to fall
    back to the Python implementation. ``ValueError`` is raised when the Rust
    helper explicitly reports an invalid backend (returncode != 0 carrying
    the structured ``error`` field) so the caller surfaces the SAME
    ``ap.error(...)`` message ``runtime._resolve_backend_and_device`` does.
    """
    if not rust_backend_enabled():
        return None
    helper = _resolve_helper()
    if helper is None:
        return None
    payload = {"op": "validate_backend", "params": {"backend": backend}}
    try:
        result = subprocess.run(
            [helper, _HELPER_SUBCOMMAND],
            input=json.dumps(payload, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=_DEFAULT_TIMEOUT_S,
            shell=False,
        )
    except Exception as exc:  # noqa: BLE001 — helper hiccup, fall back
        print(f"[rust:dictate-ops] {exc}", file=sys.stderr, flush=True)
        return None
    if result.returncode != 0:
        # On a known-invalid backend the Rust helper returns the
        # `invalid VOICEPI_STT_BACKEND=...` message verbatim on stderr —
        # raise so the caller surfaces it like the Python path does.
        # The Rust launcher (`main.rs`) wraps every error with an
        # `error: ` prefix via `eprintln!("error: {err}")`; strip it so
        # the caller's `argparse.ap.error(...)` (which adds its own
        # `error: ` prefix) doesn't end up emitting `error: error: ...`.
        err = (result.stderr or "").strip()
        if "invalid VOICEPI_STT_BACKEND" in err:
            line = err.splitlines()[-1] if err else "invalid backend"
            if line.startswith("error: "):
                line = line[len("error: "):]
            raise ValueError(line)
        if err:
            print(f"[rust:dictate-ops] {err}", file=sys.stderr, flush=True)
        return None
    try:
        body = json.loads(result.stdout or "{}")
    except Exception as exc:  # noqa: BLE001 — fall back on malformed JSON
        print(f"[rust:dictate-ops] invalid JSON: {exc}", file=sys.stderr, flush=True)
        return None
    if not isinstance(body, dict):
        return None
    canonical = body.get("backend")
    label = body.get("label")
    if not isinstance(canonical, str) or not isinstance(label, str):
        return None
    return canonical, label


def rust_changed_restart_keys(
    before: dict[str, str], after: dict[str, str]
) -> list[str] | None:
    """Return the restart-required keys whose value changed, via Rust.

    ``None`` when the Rust backend is disabled / unavailable; an empty list
    when nothing changed. Mirrors ``Dictate._report_restart_required``'s
    inner ``changed_restart`` computation so the printed warning is
    identical regardless of which side does the diff.
    """
    body = call_op(
        "changed_restart_keys",
        {
            "before": {str(k): str(v) for k, v in before.items()},
            "after": {str(k): str(v) for k, v in after.items()},
        },
    )
    if not isinstance(body, dict):
        return None
    changed = body.get("changed")
    if not isinstance(changed, list):
        return None
    return [str(k) for k in changed]


def rust_should_skip(
    *,
    samples: int,
    recording_s: float,
    min_record_seconds: float,
    parakeet_min_seconds: float,
    backend: str,
) -> str | None | tuple[None, None]:
    """Decide whether to drop a captured clip, via the Rust helper.

    Return values:
      * ``"too_short"`` when the Rust helper says drop the clip;
      * ``None`` when the helper says keep the clip;
      * ``(None, None)`` (a sentinel) when the helper is unavailable so
        the caller can distinguish "fall back to Python" from "keep".

    Intentionally low-level: the caller stays responsible for printing the
    hint + recording_s in the no-text event, so the Python orchestrator's
    output stays byte-identical when the gate is on.
    """
    body = call_op(
        "should_skip",
        {
            "samples": int(samples),
            "recording_s": float(recording_s),
            "min_record_seconds": float(min_record_seconds),
            "parakeet_min_seconds": float(parakeet_min_seconds),
            "backend": str(backend),
        },
    )
    if not isinstance(body, dict):
        return (None, None)
    reason = body.get("reason")
    if reason is None:
        return None
    return str(reason)
