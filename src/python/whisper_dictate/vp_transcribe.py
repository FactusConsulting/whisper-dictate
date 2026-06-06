"""Whisper transcription core — pure function plus hallucination filter.

Imports faster_whisper lazily inside _transcribe so the module is cheap to
import while the runtime module keeps the heavy DLL/CUDA bootstrap centralized.
"""
from __future__ import annotations

import os
import json
import re
import subprocess
import time
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from whisper_dictate.vp_audio import _boost_quiet, _looks_like_speech
from whisper_dictate.vp_config import apply_config_to_environ, get_value

apply_config_to_environ()

SR = 16000

# beam_size=1 is fastest on CPU; raise to 5 for better accuracy at the
# cost of 3-4x slower transcription. VOICEPI_BEAM_SIZE=5 is useful on
# machines without GPU where accuracy matters more than latency.
BEAM_SIZE = int(get_value("VOICEPI_BEAM_SIZE", "1") or "1")

# Optional context hint fed to Whisper before each utterance. Improves
# recognition of domain-specific terms (product names, jargon, names).
INITIAL_PROMPT = get_value("VOICEPI_INITIAL_PROMPT") or None


def _parse_temperatures(spec: str | None) -> list[float]:
    # Comma-separated floats; "0.0,0.2" by default. Set "0.0" (or "0")
    # to lock Whisper to greedy decode — eliminates the fallback to
    # higher-temperature decodes that can produce more "creative"
    # (= less faithful) text when the greedy pass hits no_speech /
    # log_prob thresholds.
    raw = (spec or "0.0,0.2").strip()
    try:
        out = [float(p.strip()) for p in raw.split(",") if p.strip()]
    except ValueError:
        out = []
    return out or [0.0, 0.2]


# Whisper decode-temperature ladder. faster-whisper retries at the next
# temperature when the previous decode trips an internal no_speech /
# log_prob threshold. Lock to "0.0" via env for predictable output.
TEMPERATURES = _parse_temperatures(get_value("VOICEPI_TEMPERATURE"))

# Pass `condition_on_previous_text=True` only on utterances longer
# than CONTEXT_MIN_SECONDS. Defaults to 0 = always False (avoids
# Whisper hallucinating continuations on short/quiet input — what
# the HALLUCINATIONS set was added to filter). Set to e.g. 5 to opt
# long utterances into context-conditioned decode, which helps
# Whisper keep word boundaries coherent across segments.
CONTEXT_MIN_SECONDS = float(get_value("VOICEPI_CONTEXT_MIN_SECONDS", "0") or "0")
VAD_THRESHOLD = float(get_value("VOICEPI_VAD_THRESHOLD", "0.3") or "0.3")
VAD_MIN_SILENCE_MS = int(get_value("VOICEPI_VAD_MIN_SILENCE_MS", "600") or "600")
VAD_SPEECH_PAD_MS = int(get_value("VOICEPI_VAD_SPEECH_PAD_MS", "200") or "200")
STT_DEBUG = (get_value("VOICEPI_STT_DEBUG") or "").strip().lower() not in (
    "", "0", "false", "no", "off")
VALID_STT_BACKENDS = ("whisper", "parakeet", "openai")
STT_BACKEND = (get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper").strip().lower()
if STT_BACKEND == "faster-whisper":
    STT_BACKEND = "whisper"


def _local_only_enabled() -> bool:
    return (get_value("VOICEPI_LOCAL_ONLY") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


def _assert_local_backend(backend: str, *, feature: str = "STT") -> None:
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if helper:
        try:
            r = subprocess.run(
                [helper, "privacy"],
                input=json.dumps({
                    "action": "assert_backend",
                    "local_only": _local_only_enabled(),
                    "backend": backend,
                    "feature": feature,
                }),
                text=True,
                encoding="utf-8",
                errors="replace",
                capture_output=True,
                timeout=5,
                shell=False,
            )
            if r.returncode == 0:
                payload = json.loads(r.stdout or "{}")
                if isinstance(payload, dict):
                    if not payload.get("ok", False):
                        raise RuntimeError(str(payload.get("error") or "local-only check failed"))
                    return
        except RuntimeError:
            raise
        except Exception:
            pass
    if _local_only_enabled() and (backend or "").strip().lower() not in (
        "whisper", "faster-whisper", "parakeet"):
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend!r}; "
            "choose a local backend or disable local-only mode.")


def load_stt_model(model_name: str, device: str, compute_type: str):
    """Load the selected STT backend lazily.

    The default path preserves the existing faster-whisper behaviour. The
    Parakeet path imports NeMo only after VOICEPI_STT_BACKEND=parakeet is set.
    """
    backend = STT_BACKEND
    _assert_local_backend(backend)
    if backend not in VALID_STT_BACKENDS:
        raise ValueError(
            "invalid VOICEPI_STT_BACKEND="
            f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    if backend == "parakeet":
        from whisper_dictate.vp_parakeet import ParakeetModel
        return ParakeetModel(model_name, device=device, compute_type=compute_type)
    if backend == "openai":
        from whisper_dictate.vp_external_api import ExternalTranscriptionModel
        return ExternalTranscriptionModel(model_name)
    from faster_whisper import WhisperModel
    return WhisperModel(model_name, device=device, compute_type=compute_type)


@dataclass
class TranscribeResult:
    text: str
    raw_text: str = ""
    duration_s: float = 0.0
    post_boost_dbfs: float | None = None
    compute_s: float = 0.0
    real_time_factor: float | None = None
    language: str | None = None
    language_probability: float | None = None
    segments: list[dict[str, Any]] = field(default_factory=list)
    gate: str = ""
    dictionary_terms: list[str] = field(default_factory=list)
    dictionary_replacements: list[dict[str, object]] = field(default_factory=list)


@dataclass
class DictionaryRuntimeResult:
    text: str = ""
    prompt: str | None = None
    terms: list[str] = field(default_factory=list)
    changes: list[dict[str, object]] = field(default_factory=list)
    term_count: int = 0
    replacement_count: int = 0
    path: str | None = None
    error: str | None = None
    enabled: bool = False


def _base_prompt_only(base_prompt: str | None) -> str | None:
    prompt = (base_prompt or "").strip()
    return prompt or None


def _dictionary_runtime(text: str = "", base_prompt: str | None = None) -> DictionaryRuntimeResult:
    fallback = DictionaryRuntimeResult(text=text, prompt=_base_prompt_only(base_prompt))
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        return fallback
    try:
        r = subprocess.run(
            [helper, "dictionary-runtime"],
            input=json.dumps({
                "base_prompt": base_prompt,
                "text": text,
            }, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=5,
            shell=False,
        )
    except Exception as e:  # noqa: BLE001 - dictation must survive helper trouble
        if STT_DEBUG:
            print(f"[dictionary] Rust helper error: {e}", flush=True)
        return fallback
    if r.returncode != 0:
        if STT_DEBUG:
            err = (r.stderr or "").strip()
            print(f"[dictionary] Rust helper failed: {err}", flush=True)
        return fallback
    try:
        payload = json.loads(r.stdout or "{}")
    except json.JSONDecodeError as e:
        if STT_DEBUG:
            print(f"[dictionary] Rust helper returned invalid JSON: {e}", flush=True)
        return fallback
    if not isinstance(payload, dict):
        return fallback

    changes = []
    for item in payload.get("changes") or []:
        if not isinstance(item, dict):
            continue
        changes.append({
            "from": str(item.get("from") or ""),
            "to": str(item.get("to") or ""),
            "count": int(item.get("count") or 0),
        })

    error = payload.get("error")
    if error and STT_DEBUG:
        print(f"[dictionary] {error}", flush=True)

    return DictionaryRuntimeResult(
        text=str(payload.get("text", text)),
        prompt=payload.get("prompt") if isinstance(payload.get("prompt"), str) else None,
        terms=[str(term) for term in (payload.get("terms") or [])],
        changes=changes,
        term_count=int(payload.get("term_count") or 0),
        replacement_count=int(payload.get("replacement_count") or 0),
        path=str(payload["path"]) if payload.get("path") else None,
        error=str(error) if error else None,
        enabled=bool(payload.get("enabled", False)),
    )

# Whisper hallucinerer disse sætninger på kort/stille lyd — ignorer dem.
HALLUCINATIONS: frozenset[str] = frozenset({
    "tak",
    "tak.",
    "tak for din opmærksomhed",
    "tak for din opmærksomhed.",
    "tak fordi du så med",
    "tak fordi du så med.",
    "tak fordi du lyttede med",
    "tak fordi du lyttede med.",
    "tak for at du så med",
    "tak for at du så med.",
    "tak for at i så med",
    "tak for at i så med.",
    "tak fordi i så med",
    "tak fordi i så med.",
    "thank you",
    "thank you.",
    "thank you for watching",
    "thank you for watching.",
    "thank you for listening",
    "thank you for listening.",
    "thanks for watching",
    "thanks for watching.",
    "undertekster af",
    "undertekstet af",
})


def is_hallucination(text: str) -> bool:
    return text.lower().rstrip() in HALLUCINATIONS


def _segment_metric(segment) -> dict[str, Any]:
    out: dict[str, Any] = {
        "start": getattr(segment, "start", None),
        "end": getattr(segment, "end", None),
        "text": getattr(segment, "text", ""),
    }
    for name in ("avg_logprob", "no_speech_prob", "compression_ratio"):
        if hasattr(segment, name):
            out[name] = getattr(segment, name)
    return out


def _transcribe_detail(model, pcm: np.ndarray, lang: str | None) -> TranscribeResult:
    # pcm: int16 mono @ 16 kHz straight from sounddevice — already the
    # rate/layout Whisper wants, so no WAV round-trip or resample. Just
    # int16 -> float32 -> boost.
    raw_audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    ok, gate = _looks_like_speech(raw_audio)
    if not ok:
        print(f"[gate] {gate}", flush=True)
        return TranscribeResult(text="", gate=gate)
    print(f"[gate] {gate}", flush=True)
    audio = _boost_quiet(raw_audio)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    use_context = CONTEXT_MIN_SECONDS > 0 and dur >= CONTEXT_MIN_SECONDS
    t0 = time.monotonic()
    dictionary_prompt = _dictionary_runtime("", INITIAL_PROMPT)
    prompt = dictionary_prompt.prompt
    segments, info = model.transcribe(
        audio,
        language=lang,
        initial_prompt=prompt,
        beam_size=BEAM_SIZE,
        temperature=TEMPERATURES,
        condition_on_previous_text=use_context,
        no_speech_threshold=0.45,
        log_prob_threshold=-1.0,
        vad_filter=True,
        vad_parameters=dict(threshold=VAD_THRESHOLD,
                            min_silence_duration_ms=VAD_MIN_SILENCE_MS,
                            speech_pad_ms=VAD_SPEECH_PAD_MS),
    )
    segment_list = list(segments)
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE tokens); strip()+
    # " ".join() drops that at segment joins -> "hørerdig". Join raw,
    # then collapse whitespace runs to one space.
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segment_list)).strip()
    dictionary_text = _dictionary_runtime(raw_text, INITIAL_PROMPT)
    text = dictionary_text.text
    replacements = dictionary_text.changes
    compute_s = time.monotonic() - t0
    rtf = compute_s / dur if dur > 0 else None
    seg_metrics = [_segment_metric(s) for s in segment_list]
    print(f"[stt] dur={dur:.1f}s post-boost={in_dbfs:.0f}dBFS "
          f"compute={compute_s:.1f}s rtf={rtf:.2f} text={text!r}", flush=True)
    if STT_DEBUG:
        for i, segment in enumerate(seg_metrics, 1):
            print(f"[stt-debug] segment#{i} {segment}", flush=True)
        if replacements:
            print(f"[stt-debug] dictionary replacements={replacements}", flush=True)
    return TranscribeResult(
        text=text,
        raw_text=raw_text,
        duration_s=dur,
        post_boost_dbfs=in_dbfs,
        compute_s=compute_s,
        real_time_factor=rtf,
        language=getattr(info, "language", None),
        language_probability=getattr(info, "language_probability", None),
        segments=seg_metrics,
        gate=gate,
        dictionary_terms=dictionary_prompt.terms,
        dictionary_replacements=replacements,
    )


def _transcribe(model, pcm: np.ndarray, lang: str | None) -> str:
    return _transcribe_detail(model, pcm, lang).text
