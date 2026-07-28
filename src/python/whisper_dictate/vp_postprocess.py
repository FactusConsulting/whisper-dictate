"""Optional local text post-processing after STT and dictionary fixes."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, field

from whisper_dictate.vp_config import apply_config_to_environ, config_snapshot, get_value
from whisper_dictate.vp_external_api import DEFAULT_OPENAI_BASE_URL, GROQ_BASE_URL, openai_chat_completion
from whisper_dictate.vp_rust import helper_path, run_json_helper
from whisper_dictate.vp_rust import no_console_window_kwargs

apply_config_to_environ()

VALID_PROCESSORS = ("none", "ollama", "openai", "groq")
VALID_MODES = ("raw", "clean", "prompt", "terminal", "slack", "email", "bullets")
DEFAULT_OLLAMA_POST_MODEL = "qwen2.5:3b"
DEFAULT_OLLAMA_BASE_URL = "http://localhost:11434"
MODE_ALIASES = {
    "bullet-list": "bullets",
    "bullet_list": "bullets",
    "bulletlist": "bullets",
}
LOCAL_HOSTS = {"localhost", "127.0.0.1", "::1"}

# Length-scaled post-processing timeout. The configured ``post_timeout_ms`` is
# the BASE/floor wall-clock budget; the effective timeout grows with the length
# of the text being cleaned (longer text => more tokens => more model time) up to
# a hard ceiling, so a long dictation no longer silently times out and falls back
# to raw, uncleaned text. These are intentionally Python-only constants (not
# user settings) — only the base/floor is configurable.
PER_CHAR_MS = 20
CEILING_MS = 30000


def effective_timeout_ms(base_ms: int, text_chars: int) -> int:
    """Length-scaled HTTP timeout for a cleanup call (pure, unit-testable).

    ``clamp(base_ms + text_chars * PER_CHAR_MS, base_ms, CEILING_MS)``: the
    configured ``base_ms`` is the floor, every character adds ``PER_CHAR_MS`` to
    the budget, and the result is capped at ``CEILING_MS``.
    """
    scaled = base_ms + max(0, int(text_chars)) * PER_CHAR_MS
    return max(base_ms, min(scaled, CEILING_MS))


@dataclass(frozen=True)
class Redaction:
    placeholder: str
    value: str
    kind: str


@dataclass(frozen=True)
class RedactionResult:
    text: str
    redactions: tuple[Redaction, ...] = field(default_factory=tuple)

    def restore(self, text: str) -> str:
        out = text
        for redaction in self.redactions:
            out = out.replace(redaction.placeholder, redaction.value)
        return out

    def public_summary(self) -> list[dict[str, object]]:
        return [
            {
                "placeholder": redaction.placeholder,
                "kind": redaction.kind,
                "chars": len(redaction.value),
            }
            for redaction in self.redactions
        ]


@dataclass(frozen=True)
class PostprocessSettings:
    processor: str = "none"
    mode: str = "raw"
    model: str = DEFAULT_OLLAMA_POST_MODEL
    base_url: str = DEFAULT_OLLAMA_BASE_URL
    timeout_ms: int = 4000
    max_input_chars: int = 4000
    max_output_chars: int = 4000
    api_key: str = ""
    # Codex P1 #642: the NORMALISED endpoint the Rust launcher resolved
    # ``api_key`` for, stamped as ``VOICEPI_POST_API_KEY_ENDPOINT``. Empty
    # when the user set the key themselves (backward-compat: nothing blocks a
    # key without a marker). When set AND the current ``base_url`` classifies
    # to a different provider, the pipeline refuses to send the key so a live
    # ``post_processor`` / ``post_base_url`` change cannot leak the stored
    # provider key to a different host.
    api_key_endpoint: str = ""
    redact: bool = False
    redact_terms: str = ""


@dataclass(frozen=True)
class PostprocessResult:
    text: str
    raw_text: str
    changed: bool
    provider: str
    mode: str
    model: str
    latency_ms: int = 0
    fallback: bool = False
    error: str = ""
    redacted: bool = False
    redactions: list[dict[str, object]] | None = None


def _int_setting(name: str, default: int, minimum: int = 0, snapshot=None) -> int:
    getter = snapshot.get_value if snapshot is not None else get_value
    try:
        return max(minimum, int(float(getter(name, str(default)) or default)))
    except (TypeError, ValueError):
        return default


def _default_base_url(processor: str) -> str:
    if processor == "groq":
        return GROQ_BASE_URL
    if processor == "openai":
        return DEFAULT_OPENAI_BASE_URL
    return DEFAULT_OLLAMA_BASE_URL


def _postprocess_api_key(snapshot=None) -> str:
    getter = snapshot.get_value if snapshot is not None else get_value
    return (
        getter("VOICEPI_POST_API_KEY")
        or getter("VOICEPI_STT_API_KEY")
        or getter("GROQ_API_KEY")
        or getter("OPENAI_API_KEY")
        or ""
    ).strip()


def _postprocess_api_key_endpoint(snapshot=None) -> str:
    """Endpoint marker the Rust launcher stamped for the injected post key.

    Populated by ``runtime::cloud_api_keys`` alongside ``VOICEPI_POST_API_KEY``
    when the key came from the credential store; empty otherwise. Consumed
    downstream by ``endpoint_marker_mismatch`` (Rust ``run.rs``'s
    ``require_endpoint_matches_marker`` mirrors the same rule for the
    in-process Rust engine).
    """
    getter = snapshot.get_value if snapshot is not None else get_value
    return (getter("VOICEPI_POST_API_KEY_ENDPOINT") or "").strip()


def _endpoint_provider(url: str) -> str:
    """Classify ``url`` by HOST (not substring) into Groq / OpenAI / Custom.

    Mirrors ``crate::credentials::Provider::from_base_url`` so the Python
    endpoint-marker check refuses / allows the same set of URLs the Rust
    launcher does. Host classification, not ``contains``: the URL
    ``https://api.groq.com@evil.example/v1`` has host ``evil.example`` and
    the URL ``https://groq.com.attacker.example/v1`` merely contains
    ``groq.com`` -- getting this wrong would hand a stored provider
    credential to an unrelated host.
    """
    try:
        parsed = urllib.parse.urlparse((url or "").strip())
    except ValueError:
        return "custom"
    host = (parsed.hostname or "").lower()
    if not host:
        return "custom"
    if host == "groq.com" or host.endswith(".groq.com"):
        return "groq"
    if host == "openai.com" or host.endswith(".openai.com"):
        return "openai"
    return "custom"


def endpoint_marker_mismatch(base_url: str, marker: str) -> str:
    """Return an error string when the marker rejects sending the key.

    ``""`` when the check passes (no marker, or same-provider). Non-empty
    when the marker was set for a different provider than ``base_url`` --
    e.g. key resolved for Groq but ``base_url`` moved to OpenAI or to a
    self-hosted host. Codex P1 #642.
    """
    marker = (marker or "").strip()
    if not marker:
        return ""
    base_provider = _endpoint_provider(base_url)
    marker_provider = _endpoint_provider(marker)
    if base_provider == marker_provider:
        return ""
    return (
        "refusing to send stored post-processing key to a different endpoint: "
        f"key was resolved for {marker!r} ({marker_provider}) but current base URL is "
        f"{base_url!r} ({base_provider}). Update the API key for the new provider in "
        "Settings, or restart the worker so the launcher resolves the right key."
    )


def _normalized_model(processor: str, raw_model: str) -> str:
    if processor == "groq" and raw_model in ("", DEFAULT_OLLAMA_POST_MODEL):
        return "llama-3.1-8b-instant"
    return raw_model or DEFAULT_OLLAMA_POST_MODEL


def _normalized_base_url(processor: str, raw_base_url: str) -> str:
    ollama_base_url = DEFAULT_OLLAMA_BASE_URL
    if processor == "groq" and raw_base_url in ("", ollama_base_url, DEFAULT_OPENAI_BASE_URL):
        return GROQ_BASE_URL
    if processor == "openai" and raw_base_url in ("", ollama_base_url, GROQ_BASE_URL):
        return DEFAULT_OPENAI_BASE_URL
    if processor == "ollama" and raw_base_url in ("", DEFAULT_OPENAI_BASE_URL, GROQ_BASE_URL):
        return ollama_base_url
    return raw_base_url


def load_postprocess_settings() -> PostprocessSettings:
    snapshot = config_snapshot()
    processor = (snapshot.get_value("VOICEPI_POST_PROCESSOR", "none") or "none").strip().lower()
    mode = normalize_mode(snapshot.get_value("VOICEPI_POST_MODE", "raw") or "raw")
    if processor not in VALID_PROCESSORS:
        processor = "none"
    if mode not in VALID_MODES:
        mode = "raw"
    raw_model = snapshot.get_value("VOICEPI_POST_MODEL") or ""
    default_base_url = _default_base_url(processor)
    raw_base_url = (snapshot.get_value("VOICEPI_POST_BASE_URL", default_base_url) or default_base_url).rstrip("/")
    return PostprocessSettings(
        processor=processor,
        mode=mode,
        model=_normalized_model(processor, raw_model),
        base_url=_normalized_base_url(processor, raw_base_url),
        timeout_ms=_int_setting("VOICEPI_POST_TIMEOUT_MS", 4000, 100, snapshot),
        max_input_chars=_int_setting("VOICEPI_POST_MAX_INPUT_CHARS", 4000, 100, snapshot),
        max_output_chars=_int_setting("VOICEPI_POST_MAX_OUTPUT_CHARS", 4000, 100, snapshot),
        api_key=_postprocess_api_key(snapshot),
        api_key_endpoint=_postprocess_api_key_endpoint(snapshot),
        redact=(snapshot.get_value("VOICEPI_POST_REDACT") or "").strip().lower() not in (
            "", "0", "false", "no", "off"),
        redact_terms=snapshot.get_value("VOICEPI_POST_REDACT_TERMS", "") or "",
    )


def _is_local_url(url: str) -> bool:
    parsed = urllib.parse.urlparse(url)
    return (parsed.hostname or "").lower() in LOCAL_HOSTS


def normalize_mode(mode: str) -> str:
    value = (mode or "raw").strip().lower()
    return MODE_ALIASES.get(value, value)


def validate_postprocess_settings(settings: PostprocessSettings) -> None:
    mode = normalize_mode(settings.mode)
    if settings.processor == "none" or mode == "raw":
        return
    if settings.processor not in VALID_PROCESSORS:
        raise ValueError(f"invalid post processor: {settings.processor}")
    if mode not in VALID_MODES:
        raise ValueError(f"invalid post mode: {settings.mode}")
    _assert_local_processor(settings.processor)
    parsed = urllib.parse.urlparse(settings.base_url)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        raise ValueError(f"invalid post-process base URL: {settings.base_url!r}")
    if _local_only_enabled() and not _is_local_url(settings.base_url):
        raise RuntimeError(
            "VOICEPI_LOCAL_ONLY=1 blocks remote post-processing URL "
            f"{settings.base_url!r}; use localhost or disable local-only mode.")


_MODE_INSTRUCTIONS = {
    "clean": (
        "Clean punctuation, casing and only obvious transcription artifacts. "
        "Preserve the speaker's wording, word order and sentence structure "
        "unless grammar is clearly broken. Do not paraphrase or add facts."
    ),
    "prompt": (
        "Rewrite into a clear, actionable prompt for an AI coding agent. "
        "Preserve constraints, technical terms and intent. Do not add facts."
    ),
    "terminal": (
        "Clean only obvious transcription artifacts. Preserve commands, flags, "
        "file paths, URLs, package names, product names, casing and code identifiers."
    ),
    "slack": (
        "Rewrite as a concise Slack-style message. Keep it natural and faithful."
    ),
    "email": (
        "Rewrite as a polished but faithful email. Preserve all concrete details."
    ),
    "bullets": (
        "Rewrite as concise bullet points. Preserve all concrete details."
    ),
}


def build_prompt(text: str, mode: str) -> str:
    mode = normalize_mode(mode)
    instruction = _MODE_INSTRUCTIONS.get(mode, _MODE_INSTRUCTIONS["clean"])
    return (
        "You are a local text post-processor for speech dictation.\n"
        f"Task: {instruction}\n"
        "Return only the rewritten text. If the input is already good, return it unchanged.\n\n"
        "Do not include the original text, labels, explanations, before/after formatting, "
        "or words such as 'becomes'.\n\n"
        f"Input:\n{text}"
    )


def _redaction_terms(settings: PostprocessSettings) -> list[str]:
    return [term.strip() for term in settings.redact_terms.split(",") if term.strip()]


def _redact_for_cloud(text: str, settings: PostprocessSettings) -> RedactionResult:
    if settings.processor not in ("openai", "groq") or not settings.redact:
        return RedactionResult(text=text)
    return _redact_text(text, terms=_redaction_terms(settings))


def _rust_json(command: str, payload: dict[str, object], *, timeout: float = 5.0) -> dict[str, object] | None:
    return run_json_helper(command, payload, timeout=timeout)


def _local_only_enabled() -> bool:
    return (get_value("VOICEPI_LOCAL_ONLY") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


def _assert_local_processor(processor: str) -> None:
    result = _rust_json("privacy", {
        "action": "assert_processor",
        "local_only": _local_only_enabled(),
        "processor": processor,
    })
    if isinstance(result, dict):
        if not result.get("ok", False):
            raise RuntimeError(str(result.get("error") or "local-only check failed"))
        return
    if _local_only_enabled() and (processor or "").strip().lower() not in ("none", "ollama"):
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks post-processing provider {processor!r}; "
            "choose a local provider or disable local-only mode.")


def _redact_text(text: str, *, terms: list[str]) -> RedactionResult:
    result = _rust_json("redact-text", {"text": text, "terms": terms})
    if not isinstance(result, dict):
        raise RuntimeError("Rust redaction helper is not available")
    return RedactionResult(
        text=str(result.get("text", text)),
        redactions=tuple(
            Redaction(
                placeholder=str(item.get("placeholder", "")),
                value=str(item.get("value", "")),
                kind=str(item.get("kind", "")),
            )
            for item in result.get("redactions", [])
            if isinstance(item, dict)
        ),
    )


def _ollama_generate(settings: PostprocessSettings, text: str) -> str:
    mode = normalize_mode(settings.mode)
    payload = {
        "model": settings.model,
        "prompt": build_prompt(text, mode),
        "stream": False,
        "options": {
            "temperature": 0,
            "num_predict": max(1, settings.max_output_chars // 4),
        },
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        settings.base_url + "/api/generate",
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=effective_timeout_ms(settings.timeout_ms, len(text)) / 1000.0) as resp:
        obj = json.loads(resp.read().decode("utf-8", errors="replace"))
    output = str(obj.get("response", "")).strip()
    return output or text


def _comparison_text(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip().casefold()


_FINAL_MARKER_RE = re.compile(
    r"(?im)^\s*(?:becomes|bliver til|rewritten|rewrite|output|final|result|cleaned|"
    r"rettet|endelig(?:\s+tekst)?)\s*:?\s*$"
)
_INLINE_FINAL_MARKER_RE = re.compile(r"\s+(?:becomes|bliver til|=>|->|→)\s+", re.IGNORECASE)


def _extract_final_text(output: str, source_text: str) -> str:
    out = (output or "").strip()
    source = (source_text or "").strip()
    if not out or not source:
        return out

    source_cmp = _comparison_text(source)
    for marker in _FINAL_MARKER_RE.finditer(out):
        prefix = out[: marker.start()]
        final = out[marker.end():].strip()
        if final and source_cmp in _comparison_text(prefix):
            return final

    for marker in _INLINE_FINAL_MARKER_RE.finditer(out):
        prefix = out[: marker.start()]
        final = out[marker.end():].strip()
        if final and _comparison_text(prefix) == source_cmp:
            return final

    return out


def _rust_postprocess_enabled() -> bool:
    # Default flipped to Rust (Python-removal roadmap #348): post-processing now
    # runs through the Rust `postprocess` verb unless the operator explicitly
    # opts back into the in-process Python path with
    # VOICEPI_POSTPROCESS_BACKEND=python (or any non-"rust" value). The Rust
    # helper is resolvable in every real run because the supervisor exports
    # VOICEPI_RUST_INJECTOR for the worker; on a build/env where it is NOT
    # resolvable, `_rust_postprocess_text` returns None and the caller falls
    # back to Python, so the flip is safe by construction.
    backend = (get_value("VOICEPI_POSTPROCESS_BACKEND") or "rust").strip().lower()
    return backend == "rust"


def _rust_postprocess_text(text: str, settings: PostprocessSettings) -> PostprocessResult | None:
    """Shell out to ``whisper-dictate postprocess`` for the full pipeline.

    Returns the parsed :class:`PostprocessResult` on success, ``None`` on any
    failure so the caller falls back to the in-process Python path. Active by
    default (VOICEPI_POSTPROCESS_BACKEND unset or ``=rust``) as long as the
    helper is resolvable from ``VOICEPI_RUST_INJECTOR``; set
    ``VOICEPI_POSTPROCESS_BACKEND=python`` to force the in-process path
    (Wave 4-B of #348, default flipped to Rust in the Python-removal roadmap).
    """
    if not _rust_postprocess_enabled():
        return None
    helper = helper_path()
    if not helper:
        return None
    payload = {
        "action": "process",
        "text": text,
        "settings": {
            "processor": settings.processor,
            "mode": settings.mode,
            "model": settings.model,
            "base_url": settings.base_url,
            "timeout_ms": int(settings.timeout_ms),
            "max_input_chars": int(settings.max_input_chars),
            "max_output_chars": int(settings.max_output_chars),
            "api_key": settings.api_key,
            # Codex P1 #642: pass the marker across the JSON envelope so the
            # Rust `postprocess` verb can refuse the injected key if the
            # worker's live endpoint no longer matches the endpoint the
            # launcher resolved it for.
            "api_key_endpoint": settings.api_key_endpoint,
            "redact": bool(settings.redact),
            "redact_terms": settings.redact_terms,
            "local_only": _local_only_enabled(),
        },
    }
    # Size the subprocess budget so the parent never kills a still-working
    # child mid-request (which would waste the provider call and double-charge
    # on the Python retry below). Normally the child's HTTP timeout is
    # length-scaled from the input, so we mirror that. But when cloud redaction
    # is enabled the child sizes its timeout from the redaction-EXPANDED prompt
    # (each short term becomes a `[[WD_TERM_n]]` placeholder), which can be much
    # longer than `text` and can push the child up to CEILING_MS; a budget
    # derived from `len(text)` alone could then kill it early. Cover the child's
    # worst case in that case.
    if settings.redact:
        budget_ms = max(int(settings.timeout_ms), CEILING_MS)
    else:
        budget_ms = effective_timeout_ms(settings.timeout_ms, len(text))
    helper_timeout = max(2.0, budget_ms / 1000.0 + 5.0)
    try:
        result = subprocess.run(
            [helper, "postprocess"],
            input=json.dumps(payload, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=helper_timeout,
            shell=False,
            **no_console_window_kwargs(),
        )
    except Exception as exc:  # noqa: BLE001 - helper failures must not break post-processing
        print(f"[rust:postprocess] {exc}", file=sys.stderr, flush=True)
        return None
    if result.returncode != 0:
        err = (result.stderr or "").strip()
        if err:
            print(f"[rust:postprocess] {err}", file=sys.stderr, flush=True)
        return None
    try:
        obj = json.loads(result.stdout or "{}")
    except Exception as exc:  # noqa: BLE001 - bad JSON is a helper bug, fall back
        print(f"[rust:postprocess] invalid JSON: {exc}", file=sys.stderr, flush=True)
        return None
    if not isinstance(obj, dict):
        return None
    # A ``fallback=true`` envelope means the Rust helper could not clean the
    # text. WHY it fell back decides whether a Python retry is safe, via the
    # ``fallback_kind`` field the helper stamps on the envelope:
    #
    #   * ``"transport"`` — the request never reached the provider (DNS /
    #     connect / TLS handshake against an enterprise CA / Windows registry
    #     proxy). Python's ``urllib`` validates TLS through the OS trust store
    #     and honours the registry proxy, so it may succeed where ureq cannot,
    #     and — because the provider was never billed — a retry cannot
    #     double-charge. Fall through to the in-process Python path by returning
    #     None. This is the general safety net for enterprise-Windows
    #     ureq/urllib parity gaps (trust store, proxy, and any future
    #     connect-phase difference), not a per-gap patch.
    #   * anything else (``"terminal"``: HTTP 401/429/500, invalid response
    #     JSON, an ambiguous client timeout, or a config/local-only rejection)
    #     — the provider was reached or the outcome is ambiguous, so Python
    #     would hit the identical result or risk a duplicate charge. Return the
    #     envelope as-is; do not retry.
    #
    # Genuine helper-level failures (crash / non-zero exit / bad JSON / a killed
    # subprocess) already returned None above.
    if bool(obj.get("fallback", False)) and str(obj.get("fallback_kind", "")) == "transport":
        detail = str(obj.get("error", "") or "")
        print(
            f"[rust:postprocess] transport fallback, retrying via Python path: {detail}",
            file=sys.stderr,
            flush=True,
        )
        return None
    return PostprocessResult(
        text=str(obj.get("text", text)),
        raw_text=str(obj.get("raw_text", text)),
        changed=bool(obj.get("changed", False)),
        provider=str(obj.get("provider", settings.processor)),
        mode=str(obj.get("mode", normalize_mode(settings.mode))),
        model=str(obj.get("model", settings.model)),
        latency_ms=int(obj.get("latency_ms", 0) or 0),
        fallback=bool(obj.get("fallback", False)),
        error=str(obj.get("error", "") or ""),
        redacted=bool(obj.get("redacted", False)),
        redactions=list(obj.get("redactions") or []) or None,
    )


def postprocess_text(text: str, settings: PostprocessSettings | None = None) -> PostprocessResult:
    settings = settings or load_postprocess_settings()
    mode = normalize_mode(settings.mode)
    if settings.processor == "none" or mode == "raw" or not text.strip():
        return PostprocessResult(
            text=text,
            raw_text=text,
            changed=False,
            provider=settings.processor,
            mode=mode,
            model=settings.model,
        )

    rust_result = _rust_postprocess_text(text, settings)
    if rust_result is not None:
        return rust_result

    validate_postprocess_settings(settings)
    # Codex P1 #642: refuse to send the injected key to a different endpoint
    # than the one the launcher resolved it for. Only applies to the cloud
    # branches (ollama has no bearer). Empty marker => user set the key
    # themselves => no check.
    if settings.processor in ("openai", "groq"):
        mismatch = endpoint_marker_mismatch(settings.base_url, settings.api_key_endpoint)
        if mismatch:
            return PostprocessResult(
                text=text,
                raw_text=text,
                changed=False,
                provider=settings.processor,
                mode=mode,
                model=settings.model,
                latency_ms=0,
                fallback=True,
                error=mismatch,
            )
    clipped = text[: settings.max_input_chars]
    redaction = _redact_for_cloud(clipped, settings)
    prompt_text = redaction.text
    redaction_summary = redaction.public_summary()
    t0 = time.monotonic()
    try:
        if settings.processor == "ollama":
            out = _ollama_generate(settings, clipped)
            latency_ms = int((time.monotonic() - t0) * 1000)
        elif settings.processor in ("openai", "groq"):
            # The configured timeout is the BASE/floor; scale the effective HTTP
            # budget with the length of the text being cleaned so longer
            # dictations get proportionally more time (up to CEILING_MS) instead
            # of silently timing out and falling back to raw text.
            out, latency_ms = openai_chat_completion(
                base_url=settings.base_url,
                api_key=settings.api_key,
                model=settings.model,
                prompt=build_prompt(prompt_text, mode),
                timeout_ms=effective_timeout_ms(settings.timeout_ms, len(prompt_text)),
            )
        else:
            raise ValueError(f"unsupported post processor: {settings.processor}")
        out = _extract_final_text(out, prompt_text)
        if redaction.redactions:
            out = redaction.restore(out)
        out = out[: settings.max_output_chars].strip() or text
        return PostprocessResult(
            text=out,
            raw_text=text,
            changed=out != text,
            provider=settings.processor,
            mode=mode,
            model=settings.model,
            latency_ms=latency_ms,
            redacted=bool(redaction.redactions),
            redactions=redaction_summary,
        )
    # OSError covers TimeoutError + urllib.error.URLError; ValueError covers
    # json.JSONDecodeError — listing the subclasses would be redundant.
    except (OSError, RuntimeError, ValueError) as exc:
        latency_ms = int((time.monotonic() - t0) * 1000)
        return PostprocessResult(
            text=text,
            raw_text=text,
            changed=False,
            provider=settings.processor,
            mode=mode,
            model=settings.model,
            latency_ms=latency_ms,
            fallback=True,
            error=str(exc),
            redacted=bool(redaction.redactions),
            redactions=redaction_summary,
        )
