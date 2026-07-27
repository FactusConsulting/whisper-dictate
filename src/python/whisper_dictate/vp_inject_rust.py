"""Phase 2.1 (issue #348) Rust-injector shell-out helpers.

Lives in its own module so ``vp_inject.py`` stays under the 500-LOC repo
limit. Responsibilities:

* :func:`rust_injection_backend_enabled` — the on/off switch driven by the
  ``VOICEPI_INJECTION_BACKEND`` environment variable.
* :func:`inject_via_rust` — JSON-envelope subprocess call into
  ``whisper-dictate inject``. Returns a bool so the caller can fall back to
  the existing Python path on any failure (binary missing, transport error,
  helper-reported ``ok=False``).
* :func:`resolve_rust_inject_mode` — auto-strategy resolver shared with
  the Python path. Hoisted out of ``vp_inject.py`` so it stays unit-testable
  without a Dictate instance and so the mixin file stays slim.
* :func:`populate_clipboard_for_rust_paste` — copies text into pyperclip
  (with a delayed restore) so the Rust paste backend, which only sends the
  Ctrl+V keystroke, has something to paste.

Module-level functions so unit tests can drive them directly with a stubbed
``subprocess.run`` — no mixin instance required.
"""
from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass
from whisper_dictate.vp_rust import no_console_window_kwargs
import threading
from typing import Callable, Optional


# Public env contract — also referenced from vp_inject.py for the dispatch
# branch in _inject(). Keeping the constants here means the env-var name is
# defined in exactly one place.
RUST_INJECTION_BACKEND_ENV = "VOICEPI_INJECTION_BACKEND"
RUST_INJECTION_BACKEND_VALUE = "rust"
RUST_INJECTOR_BINARY_ENV = "VOICEPI_RUST_INJECTOR"


@dataclass(frozen=True)
class RustInjectResult:
    """Full outcome of a ``whisper-dictate inject`` invocation.

    ``ok`` — the injection succeeded end-to-end.
    ``partial`` — Rust reports it typed at least one keystroke before
    failing. The outer Python fallback (``vp_inject._inject``) MUST NOT
    re-inject the same text when this is set, or the successful prefix
    will be double-typed into the user's document. Codex P1 #613
    dispatcher.rs:599.

    ``__bool__`` returns ``ok`` so the legacy bool-shaped call sites
    (``if inject_via_rust(...)``) continue to behave the same way.
    """

    ok: bool
    partial: bool = False

    def __bool__(self) -> bool:  # legacy call sites
        return self.ok


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
) -> RustInjectResult:
    """Shell out to ``whisper-dictate inject`` with a JSON request envelope.

    Returns a :class:`RustInjectResult` carrying ``ok`` and ``partial``.
    ``bool(result)`` matches ``result.ok`` so the legacy ``if
    inject_via_rust(...)`` call sites keep working. Callers that need to
    prevent the Python outer fallback from re-injecting after a partial
    Rust burst (Codex P1 #613) must read ``result.partial`` -- see
    :meth:`whisper_dictate.vp_inject._Inject._inject_via_rust_backend`.

    Any failure (binary missing, subprocess error, JSON parse error,
    helper-reported ``ok=False``) surfaces as ``RustInjectResult(ok=False,
    partial=...)``. The envelope matches the Rust
    ``dispatcher::InjectRequest::Inject`` variant — see
    ``src/rust/injection/dispatcher.rs`` for the schema.
    """
    binary = helper or os.environ.get(RUST_INJECTOR_BINARY_ENV)
    if not binary:
        return RustInjectResult(ok=False)
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
            **no_console_window_kwargs(),
        )
    except Exception as exc:
        print(f"[inject] rust injector launch failed: {exc}", flush=True)
        return RustInjectResult(ok=False)
    if completed.returncode != 0:
        err = completed.stderr.decode(errors="replace").strip()
        if err:
            print(f"[inject] rust injector exited {completed.returncode}: {err}", flush=True)
        # The Rust CLI only reaches non-zero for launch / parse errors
        # before any injection is attempted, so partial cannot be true
        # here even if a future revision were to try.
        return RustInjectResult(ok=False)
    try:
        response = json.loads(completed.stdout.decode("utf-8") or "{}")
    except json.JSONDecodeError as exc:
        print(f"[inject] rust injector returned invalid JSON: {exc}", flush=True)
        return RustInjectResult(ok=False)
    ok = bool(response.get("ok"))
    partial = bool(response.get("partial"))
    if not ok:
        err = response.get("error") or "unknown error"
        suffix = " (partial burst -- suppressing Python fallback)" if partial else ""
        print(f"[inject] rust injector reported failure{suffix}: {err}", flush=True)
    return RustInjectResult(ok=ok, partial=partial)


def resolve_rust_inject_mode(
    user_mode: str,
    text: str,
    *,
    on_wayland: bool,
    wayland_text_prefers_paste: Callable[[str], bool],
    text_prefers_paste: Callable[[str], bool],
    target_prefers_paste: Callable[[], bool],
    ptt_is_bare_modifier: Callable[[], bool],
) -> str:
    """Pick ``"paste"`` or ``"type"`` for the Rust dispatcher.

    Codex P2 (PR #351): the original delegation collapsed ``auto`` to
    ``paste`` unconditionally, losing the per-text / per-target heuristics
    that the Python path used. This mirrors ``_inject_other`` /
    ``_inject_wayland`` so the Rust path picks the same strategy the Python
    path would have. The predicate callables let the mixin keep its existing
    methods without exposing them as module-level functions.
    """
    if user_mode == "paste":
        return "paste"
    if user_mode == "type":
        return "type"
    # user_mode == "auto"
    if on_wayland:
        return "paste" if (
            wayland_text_prefers_paste(text) or ptt_is_bare_modifier()
        ) else "type"
    return "paste" if (
        os.name == "nt"
        or target_prefers_paste()
        or text_prefers_paste(text)
        or ptt_is_bare_modifier()
    ) else "type"


def populate_clipboard_for_rust_paste(
    text: str,
    *,
    restore_enabled: bool = True,
    restore_after_delay: Optional[Callable] = None,
) -> bool:
    """Copy ``text`` to the clipboard before delegating paste to Rust.

    Codex P1 (PR #351): the Rust paste backend only sends the Ctrl+V
    keystroke — it doesn't own the clipboard. Without this step the
    delegated paste fired on an empty (or stale) clipboard. Returns
    True if the clipboard was populated; False (and skips the restore
    thread) when pyperclip is unavailable so the caller can decide
    whether to fall back to the Python path.
    """
    try:
        import pyperclip  # type: ignore[import-not-found]
    except Exception as exc:
        print(f"[inject] pyperclip unavailable for rust paste: {exc}", flush=True)
        return False
    try:
        previous: Optional[str] = None
        try:
            previous = pyperclip.paste()
        except Exception:
            pass
        pyperclip.copy(text)
        if restore_enabled and previous is not None and restore_after_delay is not None:
            t = threading.Thread(
                target=restore_after_delay,
                args=(pyperclip, text, previous),
                daemon=True,
            )
            t.start()
        return True
    except Exception as exc:
        print(f"[inject] clipboard populate for rust paste failed: {exc}", flush=True)
        return False
