"""Phase 2.1 (issue #348) Rust-injector shell-out helpers.

Lives in its own module so ``vp_inject.py`` stays under the 500-LOC repo
limit. Two responsibilities:

* :func:`rust_injection_backend_enabled` — the on/off switch driven by the
  ``VOICEPI_INJECTION_BACKEND`` environment variable.
* :func:`inject_via_rust` — JSON-envelope subprocess call into
  ``whisper-dictate inject``. Returns a bool so the caller can fall back to
  the existing Python path on any failure (binary missing, transport error,
  helper-reported ``ok=False``).

Module-level functions so unit tests can drive them directly with a stubbed
``subprocess.run`` — no mixin instance required.
"""
from __future__ import annotations

import json
import os
import subprocess


# Public env contract — also referenced from vp_inject.py for the dispatch
# branch in _inject(). Keeping the constants here means the env-var name is
# defined in exactly one place.
RUST_INJECTION_BACKEND_ENV = "VOICEPI_INJECTION_BACKEND"
RUST_INJECTION_BACKEND_VALUE = "rust"
RUST_INJECTOR_BINARY_ENV = "VOICEPI_RUST_INJECTOR"


def rust_injection_backend_enabled() -> bool:
    """True iff the user opted into the Phase 2.1 Rust injector path.

    Set ``VOICEPI_INJECTION_BACKEND=rust`` to flip the whole inject path to
    the Rust helper. Any other value (including unset) keeps the Python path
    untouched, so the default behaviour is byte-identical to today.
    """
    raw = os.environ.get(RUST_INJECTION_BACKEND_ENV, "").strip().lower()
    return raw == RUST_INJECTION_BACKEND_VALUE


def inject_via_rust(
    text: str,
    *,
    mode: str = "paste",
    shortcut: str | None = None,
    target_title: str | None = None,
    target_process: str | None = None,
    xkb_layout: str | None = None,
    helper: str | None = None,
    timeout: float = 10.0,
) -> bool:
    """Shell out to ``whisper-dictate inject`` with a JSON request envelope.

    Returns True on success, False on any failure (binary missing, subprocess
    error, JSON parse error, helper-reported ``ok=False``). The envelope
    matches the Rust ``dispatcher::InjectRequest::Inject`` variant — see
    ``src/rust/injection/dispatcher.rs`` for the schema.
    """
    binary = helper or os.environ.get(RUST_INJECTOR_BINARY_ENV)
    if not binary:
        return False
    request = {
        "action": "inject",
        "text": text,
        "method": {"mode": mode, "shortcut": shortcut or ""},
        "target_title": target_title or "",
        "target_process": target_process or "",
        "xkb_layout": xkb_layout or "",
    }
    try:
        completed = subprocess.run(
            [binary, "inject"],
            input=json.dumps(request).encode("utf-8"),
            capture_output=True,
            timeout=timeout,
        )
    except Exception as exc:
        print(f"[inject] rust injector launch failed: {exc}", flush=True)
        return False
    if completed.returncode != 0:
        err = completed.stderr.decode(errors="replace").strip()
        if err:
            print(f"[inject] rust injector exited {completed.returncode}: {err}", flush=True)
        return False
    try:
        response = json.loads(completed.stdout.decode("utf-8") or "{}")
    except json.JSONDecodeError as exc:
        print(f"[inject] rust injector returned invalid JSON: {exc}", flush=True)
        return False
    ok = bool(response.get("ok"))
    if not ok:
        err = response.get("error") or "unknown error"
        print(f"[inject] rust injector reported failure: {err}", flush=True)
    return ok
