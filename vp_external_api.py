"""External HTTP API adapters for transcription and text post-processing."""
from __future__ import annotations

import io
import json
import os
import time
import uuid
import wave
from dataclasses import dataclass
from typing import Any
import urllib.error
import urllib.parse
import urllib.request

import numpy as np

from vp_config import get_value

SR = 16000


DEFAULT_OPENAI_BASE_URL = "https://api.openai.com/v1"
LOCAL_WHISPER_MODEL_NAMES = {
    "tiny", "base", "small", "medium", "large-v3", "large-v3-turbo",
    "distil-large-v3",
}


@dataclass(frozen=True)
class ExternalApiSettings:
    provider: str
    model: str
    base_url: str
    api_key: str
    timeout_ms: int


@dataclass
class ExternalSegment:
    text: str
    start: float | None = None
    end: float | None = None


@dataclass
class ExternalInfo:
    language: str | None = None
    language_probability: float | None = None


def _int_value(name: str, default: int, minimum: int = 100) -> int:
    try:
        return max(minimum, int(float(get_value(name, str(default)) or default)))
    except (TypeError, ValueError):
        return default


def _api_key(primary_env: str) -> str:
    return (
        get_value(primary_env)
        or os.environ.get(primary_env)
        or os.environ.get("OPENAI_API_KEY")
        or ""
    ).strip()


def load_stt_api_settings(model_name: str) -> ExternalApiSettings:
    configured_model = get_value("VOICEPI_STT_MODEL")
    if not configured_model and (model_name or "").strip() in LOCAL_WHISPER_MODEL_NAMES:
        model_name = "gpt-4o-mini-transcribe"
    return ExternalApiSettings(
        provider="openai",
        model=configured_model or model_name or "gpt-4o-mini-transcribe",
        base_url=(get_value("VOICEPI_STT_BASE_URL", DEFAULT_OPENAI_BASE_URL)
                  or DEFAULT_OPENAI_BASE_URL).rstrip("/"),
        api_key=_api_key("VOICEPI_STT_API_KEY"),
        timeout_ms=_int_value("VOICEPI_STT_TIMEOUT_MS", 30000, 1000),
    )


def _require_api_key(settings: ExternalApiSettings) -> None:
    if not settings.api_key:
        raise RuntimeError(
            f"{settings.provider} API requires OPENAI_API_KEY or VOICEPI_STT_API_KEY/VOICEPI_POST_API_KEY")


def _request_json(
    url: str,
    *,
    payload: dict[str, Any] | None = None,
    data: bytes | None = None,
    headers: dict[str, str] | None = None,
    timeout_ms: int,
) -> dict[str, Any]:
    body = data if data is not None else json.dumps(payload or {}).encode("utf-8")
    req = urllib.request.Request(url, data=body, headers=headers or {}, method="POST")
    with urllib.request.urlopen(req, timeout=timeout_ms / 1000.0) as resp:
        return json.loads(resp.read().decode("utf-8", errors="replace"))


def _wav_bytes(audio: np.ndarray) -> bytes:
    clipped = np.clip(audio.reshape(-1), -1.0, 1.0)
    pcm = (clipped * 32767.0).astype(np.int16)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SR)
        wav.writeframes(pcm.tobytes())
    return buf.getvalue()


def _multipart_form(fields: dict[str, str], files: dict[str, tuple[str, bytes, str]]) -> tuple[bytes, str]:
    boundary = "----whisper-dictate-" + uuid.uuid4().hex
    chunks: list[bytes] = []
    for name, value in fields.items():
        chunks.append(f"--{boundary}\r\n".encode("ascii"))
        chunks.append(f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode("utf-8"))
        chunks.append(str(value).encode("utf-8"))
        chunks.append(b"\r\n")
    for name, (filename, content, content_type) in files.items():
        chunks.append(f"--{boundary}\r\n".encode("ascii"))
        chunks.append(
            f'Content-Disposition: form-data; name="{name}"; filename="{filename}"\r\n'.encode("utf-8"))
        chunks.append(f"Content-Type: {content_type}\r\n\r\n".encode("ascii"))
        chunks.append(content)
        chunks.append(b"\r\n")
    chunks.append(f"--{boundary}--\r\n".encode("ascii"))
    return b"".join(chunks), boundary


class ExternalTranscriptionModel:
    def __init__(self, model_name: str):
        self.settings = load_stt_api_settings(model_name)
        _require_api_key(self.settings)

    def transcribe(self, audio, **kwargs):
        language = kwargs.get("language")
        prompt = kwargs.get("initial_prompt")
        fields = {"model": self.settings.model}
        if language:
            fields["language"] = str(language)
        if prompt:
            fields["prompt"] = str(prompt)
        body, boundary = _multipart_form(
            fields,
            {"file": ("audio.wav", _wav_bytes(audio), "audio/wav")},
        )
        obj = _request_json(
            self.settings.base_url + "/audio/transcriptions",
            data=body,
            headers={
                "Authorization": f"Bearer {self.settings.api_key}",
                "Content-Type": f"multipart/form-data; boundary={boundary}",
            },
            timeout_ms=self.settings.timeout_ms,
        )
        text = str(obj.get("text", "")).strip()
        return [ExternalSegment(text=(" " + text) if text else "")], ExternalInfo(
            language=obj.get("language") if isinstance(obj.get("language"), str) else language,
        )


def openai_chat_completion(
    *,
    base_url: str,
    api_key: str,
    model: str,
    prompt: str,
    timeout_ms: int,
) -> tuple[str, int]:
    settings = ExternalApiSettings("openai", model, base_url.rstrip("/"), api_key, timeout_ms)
    _require_api_key(settings)
    t0 = time.monotonic()
    obj = _request_json(
        settings.base_url + "/chat/completions",
        payload={
            "model": settings.model,
            "messages": [
                {"role": "system", "content": "You rewrite dictated text faithfully."},
                {"role": "user", "content": prompt},
            ],
            "temperature": 0,
        },
        headers={
            "Authorization": f"Bearer {settings.api_key}",
            "Content-Type": "application/json",
        },
        timeout_ms=settings.timeout_ms,
    )
    latency_ms = int((time.monotonic() - t0) * 1000)
    choices = obj.get("choices") or []
    if not choices:
        return "", latency_ms
    message = choices[0].get("message") if isinstance(choices[0], dict) else {}
    return str((message or {}).get("content", "")).strip(), latency_ms
