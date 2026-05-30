"""Whisper transcription core — pure function plus hallucination filter.

Extracted from voice_pi.py so transcribe-only changes don't churn the
main module. Imports faster_whisper lazily inside _transcribe so the
module is cheap to import (the heavy DLL/CUDA bootstrap stays in
voice_pi.py)."""
from __future__ import annotations

import os
import re
import time
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from vp_audio import _boost_quiet, _looks_like_speech
from vp_config import apply_config_to_environ, get_value
from vp_dictionary import DICTIONARY
from vp_privacy import apply_local_only_network_lock, assert_local_backend

apply_config_to_environ()
apply_local_only_network_lock()

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
STT_DEBUG = (get_value("VOICEPI_STT_DEBUG") or "").strip().lower() not in (
    "", "0", "false", "no", "off")
VALID_STT_BACKENDS = ("whisper", "parakeet")
STT_BACKEND = (get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper").strip().lower()
if STT_BACKEND == "faster-whisper":
    STT_BACKEND = "whisper"


def load_stt_model(model_name: str, device: str, compute_type: str):
    """Load the selected STT backend lazily.

    The default path preserves the existing faster-whisper behaviour. The
    Parakeet path imports NeMo only after VOICEPI_STT_BACKEND=parakeet is set.
    """
    backend = STT_BACKEND
    assert_local_backend(backend)
    if backend not in VALID_STT_BACKENDS:
        raise ValueError(
            "invalid VOICEPI_STT_BACKEND="
            f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    if backend == "parakeet":
        from vp_parakeet import ParakeetModel
        return ParakeetModel(model_name, device=device, compute_type=compute_type)
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
    prompt = DICTIONARY.build_prompt(INITIAL_PROMPT)
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
                            min_silence_duration_ms=VAD_MIN_SILENCE_MS),
    )
    segment_list = list(segments)
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE tokens); strip()+
    # " ".join() drops that at segment joins -> "hørerdig". Join raw,
    # then collapse whitespace runs to one space.
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segment_list)).strip()
    text, replacements = DICTIONARY.apply_replacements(raw_text)
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
        dictionary_terms=DICTIONARY.prompt_terms(),
        dictionary_replacements=replacements,
    )


def _transcribe(model, pcm: np.ndarray, lang: str | None) -> str:
    return _transcribe_detail(model, pcm, lang).text
