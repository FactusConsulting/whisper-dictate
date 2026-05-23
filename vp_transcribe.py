"""Whisper transcription core — pure function plus hallucination filter.

Extracted from voice_pi.py so transcribe-only changes don't churn the
main module. Imports faster_whisper lazily inside _transcribe so the
module is cheap to import (the heavy DLL/CUDA bootstrap stays in
voice_pi.py)."""
from __future__ import annotations

import os
import re
import time

import numpy as np

from vp_audio import _boost_quiet, _looks_like_speech

SR = 16000

# beam_size=1 is fastest on CPU; raise to 5 for better accuracy at the
# cost of 3-4x slower transcription. VOICEPI_BEAM_SIZE=5 is useful on
# machines without GPU where accuracy matters more than latency.
BEAM_SIZE = int(os.environ.get("VOICEPI_BEAM_SIZE", "1"))

# Optional context hint fed to Whisper before each utterance. Improves
# recognition of domain-specific terms (product names, jargon, names).
INITIAL_PROMPT = os.environ.get("VOICEPI_INITIAL_PROMPT") or None

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


def _transcribe(model, pcm: np.ndarray, lang: str | None) -> str:
    # pcm: int16 mono @ 16 kHz straight from sounddevice — already the
    # rate/layout Whisper wants, so no WAV round-trip or resample. Just
    # int16 -> float32 -> boost.
    raw_audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    ok, gate = _looks_like_speech(raw_audio)
    if not ok:
        print(f"[gate] {gate}", flush=True)
        return ""
    print(f"[gate] {gate}", flush=True)
    audio = _boost_quiet(raw_audio)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    t0 = time.monotonic()
    segments, _ = model.transcribe(
        audio,
        language=lang,
        initial_prompt=INITIAL_PROMPT,
        beam_size=BEAM_SIZE,
        temperature=[0.0, 0.2],
        condition_on_previous_text=False,
        no_speech_threshold=0.45,
        log_prob_threshold=-1.0,
        vad_filter=True,
        vad_parameters=dict(threshold=0.3, min_silence_duration_ms=600),
    )
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE tokens); strip()+
    # " ".join() drops that at segment joins -> "hørerdig". Join raw,
    # then collapse whitespace runs to one space.
    text = re.sub(r"\s+", " ", "".join(s.text for s in segments)).strip()
    print(f"[stt] dur={dur:.1f}s post-boost={in_dbfs:.0f}dBFS "
          f"compute={time.monotonic() - t0:.1f}s text={text!r}", flush=True)
    return text
