"""Optional local text post-processing after STT and dictionary fixes."""
from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass

from whisper_dictate.vp_config import apply_config_to_environ, get_value
from whisper_dictate.vp_external_api import DEFAULT_OPENAI_BASE_URL, GROQ_BASE_URL, openai_chat_completion
from whisper_dictate.vp_privacy import assert_local_processor, local_only_enabled
from whisper_dictate.vp_redaction import RedactionResult, redact_text

apply_config_to_environ()

VALID_PROCESSORS = ("none", "ollama", "openai", "groq")
VALID_MODES = ("raw", "clean", "prompt", "terminal", "slack", "email", "bullets")
DEFAULT_OLLAMA_POST_MODEL = "qwen2.5:3b"
MODE_ALIASES = {
    "bullet-list": "bullets",
    "bullet_list": "bullets",
    "bulletlist": "bullets",
}
LOCAL_HOSTS = {"localhost", "127.0.0.1", "::1"}


@dataclass(frozen=True)
class PostprocessSettings:
    processor: str = "none"
    mode: str = "raw"
    model: str = DEFAULT_OLLAMA_POST_MODEL
    base_url: str = "http://localhost:11434"
    timeout_ms: int = 2000
    max_input_chars: int = 4000
    max_output_chars: int = 4000
    api_key: str = ""
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


def _int_setting(name: str, default: int, minimum: int = 0) -> int:
    try:
        return max(minimum, int(float(get_value(name, str(default)) or default)))
    except (TypeError, ValueError):
        return default


def _default_base_url(processor: str) -> str:
    if processor == "groq":
        return GROQ_BASE_URL
    if processor == "openai":
        return DEFAULT_OPENAI_BASE_URL
    return "http://localhost:11434"


def _postprocess_api_key() -> str:
    return (
        get_value("VOICEPI_POST_API_KEY")
        or get_value("VOICEPI_STT_API_KEY")
        or get_value("GROQ_API_KEY")
        or get_value("OPENAI_API_KEY")
        or ""
    ).strip()


def _normalized_model(processor: str, raw_model: str) -> str:
    if processor == "groq" and raw_model in ("", DEFAULT_OLLAMA_POST_MODEL):
        return "llama-3.1-8b-instant"
    return raw_model or DEFAULT_OLLAMA_POST_MODEL


def _normalized_base_url(processor: str, raw_base_url: str) -> str:
    ollama_base_url = "http://localhost:11434"
    if processor == "groq" and raw_base_url in ("", ollama_base_url, DEFAULT_OPENAI_BASE_URL):
        return GROQ_BASE_URL
    if processor == "openai" and raw_base_url in ("", ollama_base_url, GROQ_BASE_URL):
        return DEFAULT_OPENAI_BASE_URL
    if processor == "ollama" and raw_base_url in ("", DEFAULT_OPENAI_BASE_URL, GROQ_BASE_URL):
        return ollama_base_url
    return raw_base_url


def load_postprocess_settings() -> PostprocessSettings:
    processor = (get_value("VOICEPI_POST_PROCESSOR", "none") or "none").strip().lower()
    mode = normalize_mode(get_value("VOICEPI_POST_MODE", "raw") or "raw")
    if processor not in VALID_PROCESSORS:
        processor = "none"
    if mode not in VALID_MODES:
        mode = "raw"
    raw_model = get_value("VOICEPI_POST_MODEL") or ""
    default_base_url = _default_base_url(processor)
    raw_base_url = (get_value("VOICEPI_POST_BASE_URL", default_base_url) or default_base_url).rstrip("/")
    return PostprocessSettings(
        processor=processor,
        mode=mode,
        model=_normalized_model(processor, raw_model),
        base_url=_normalized_base_url(processor, raw_base_url),
        timeout_ms=_int_setting("VOICEPI_POST_TIMEOUT_MS", 2000, 100),
        max_input_chars=_int_setting("VOICEPI_POST_MAX_INPUT_CHARS", 4000, 100),
        max_output_chars=_int_setting("VOICEPI_POST_MAX_OUTPUT_CHARS", 4000, 100),
        api_key=_postprocess_api_key(),
        redact=(get_value("VOICEPI_POST_REDACT") or "").strip().lower() not in (
            "", "0", "false", "no", "off"),
        redact_terms=get_value("VOICEPI_POST_REDACT_TERMS", "") or "",
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
    assert_local_processor(settings.processor)
    parsed = urllib.parse.urlparse(settings.base_url)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        raise ValueError(f"invalid post-process base URL: {settings.base_url!r}")
    if local_only_enabled() and not _is_local_url(settings.base_url):
        raise RuntimeError(
            "VOICEPI_LOCAL_ONLY=1 blocks remote post-processing URL "
            f"{settings.base_url!r}; use localhost or disable local-only mode.")


_MODE_INSTRUCTIONS = {
    "clean": (
        "Clean punctuation, casing and obvious grammar while preserving the "
        "speaker's exact meaning. Do not add new facts."
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
        f"Input:\n{text}"
    )


def _redaction_terms(settings: PostprocessSettings) -> list[str]:
    return [term.strip() for term in settings.redact_terms.split(",") if term.strip()]


def _redact_for_cloud(text: str, settings: PostprocessSettings) -> RedactionResult:
    if settings.processor not in ("openai", "groq") or not settings.redact:
        return RedactionResult(text=text)
    return redact_text(text, terms=_redaction_terms(settings))


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
    with urllib.request.urlopen(req, timeout=settings.timeout_ms / 1000.0) as resp:
        obj = json.loads(resp.read().decode("utf-8", errors="replace"))
    output = str(obj.get("response", "")).strip()
    return output or text


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

    validate_postprocess_settings(settings)
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
            out, latency_ms = openai_chat_completion(
                base_url=settings.base_url,
                api_key=settings.api_key,
                model=settings.model,
                prompt=build_prompt(prompt_text, mode),
                timeout_ms=settings.timeout_ms,
            )
        else:
            raise ValueError(f"unsupported post processor: {settings.processor}")
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
    except (OSError, TimeoutError, urllib.error.URLError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
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
