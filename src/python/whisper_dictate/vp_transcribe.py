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

from whisper_dictate.vp_audio import _boost_quiet, _boost_quiet_detail, _looks_like_speech
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
CONTEXT_MIN_SECONDS = float(get_value("VOICEPI_CONTEXT_MIN_SECONDS", "5") or "5")
VAD_THRESHOLD = float(get_value("VOICEPI_VAD_THRESHOLD", "0.3") or "0.3")
VAD_MIN_SILENCE_MS = int(get_value("VOICEPI_VAD_MIN_SILENCE_MS", "600") or "600")
VAD_SPEECH_PAD_MS = int(get_value("VOICEPI_VAD_SPEECH_PAD_MS", "200") or "200")

# --- Trailing-silence hallucination guards (local Whisper only) ---
# When on (default), ask faster-whisper to skip long silent gaps where it tends
# to hallucinate "like and subscribe"-style text. This needs word timestamps
# (small extra compute). Toggled from the UI; a no-op for cloud STT / Parakeet,
# which never reach this code path.
HALLUCINATION_GUARD = (get_value("VOICEPI_HALLUCINATION_GUARD", "1") or "1").strip().lower() not in (
    "", "0", "false", "no", "off")
HALLUCINATION_SILENCE_S = float(get_value("VOICEPI_HALLUCINATION_SILENCE_S", "2.0") or "2.0")
# Always-on segment scrub (cheap, no setting): drop a transcribed segment the
# model itself flags as very likely non-speech (high no_speech_prob AND low
# confidence), or whose end timestamp runs past the captured audio — the classic
# "like and subscribe" / repetition artifacts Whisper emits on trailing silence.
NO_SPEECH_DROP = float(get_value("VOICEPI_NO_SPEECH_DROP", "0.6") or "0.6")
NO_SPEECH_DROP_LOGPROB = float(get_value("VOICEPI_NO_SPEECH_DROP_LOGPROB", "-0.5") or "-0.5")
SEGMENT_END_SLACK_S = 1.0
STT_DEBUG = (get_value("VOICEPI_STT_DEBUG") or "").strip().lower() not in (
    "", "0", "false", "no", "off")
VALID_STT_BACKENDS = ("whisper", "parakeet", "openai")
STT_BACKEND = (get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper").strip().lower()
if STT_BACKEND == "faster-whisper":
    STT_BACKEND = "whisper"


def _local_only_enabled() -> bool:
    return (get_value("VOICEPI_LOCAL_ONLY") or "").strip().lower() not in (
        "", "0", "false", "no", "off")


def _is_loopback_url(url: str | None) -> bool:
    """True when an HTTP(S) URL targets the local machine (loopback).

    A self-hosted endpoint on loopback never leaves the box, so it stays
    compatible with VOICEPI_LOCAL_ONLY. Mirrors the Rust privacy helper.
    """
    if not url:
        return False
    authority = url.split("://", 1)[-1].split("/", 1)[0]
    host_port = authority.rsplit("@", 1)[-1]  # strip any user:pass@
    if host_port.startswith("["):  # [::1]:port
        host = host_port[1:].split("]", 1)[0]
    else:
        host = host_port.split(":", 1)[0]
    host = host.strip().lower()
    return host in ("localhost", "::1") or host == "127.0.0.1" or host.startswith("127.")


def _rust_privacy_ok(helper: str, backend: str, feature: str,
                     base_url: str | None = None) -> bool:
    """Ask the Rust privacy helper whether ``backend`` is allowed.

    Returns True when the helper explicitly approves; raises RuntimeError when
    it explicitly rejects; returns False when the helper is unavailable or its
    answer is unusable (the caller then applies the Python fallback check).
    """
    try:
        r = subprocess.run(
            [helper, "privacy"],
            input=json.dumps({
                "action": "assert_backend",
                "local_only": _local_only_enabled(),
                "backend": backend,
                "feature": feature,
                "base_url": base_url or "",
            }),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=5,
            shell=False,
        )
    except Exception:  # noqa: BLE001 - fall back to the Python check
        return False
    if r.returncode != 0:
        return False
    try:
        payload = json.loads(r.stdout or "{}")
    except json.JSONDecodeError:
        return False
    if not isinstance(payload, dict):
        return False
    if not payload.get("ok", False):
        raise RuntimeError(str(payload.get("error") or "local-only check failed"))
    return True


def _assert_local_backend(backend: str, *, feature: str = "STT") -> None:
    base_url = get_value("VOICEPI_STT_BASE_URL")
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if helper and _rust_privacy_ok(helper, backend, feature, base_url):
        return
    if (_local_only_enabled()
            and (backend or "").strip().lower() not in ("whisper", "faster-whisper", "parakeet")
            and not _is_loopback_url(base_url)):
        raise RuntimeError(
            f"VOICEPI_LOCAL_ONLY=1 blocks {feature} backend {backend!r}; "
            "choose a local backend, a loopback endpoint, or disable local-only mode.")


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
    raw_dbfs: float | None = None
    peak: float | None = None
    gain: float | None = None
    noise_dbfs: float | None = None
    snr_db: float | None = None
    input_status: str = ""
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


def _run_dictionary_helper_payload(text: str, base_prompt: str | None) -> dict | None:
    """Run the Rust dictionary-runtime helper and return its parsed dict.

    Returns None when the helper is unavailable, exits non-zero, or returns
    unparseable / non-dict output — the caller falls back in every such case.
    """
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        return None
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
        return None
    if r.returncode != 0:
        if STT_DEBUG:
            err = (r.stderr or "").strip()
            print(f"[dictionary] Rust helper failed: {err}", flush=True)
        return None
    try:
        payload = json.loads(r.stdout or "{}")
    except json.JSONDecodeError as e:
        if STT_DEBUG:
            print(f"[dictionary] Rust helper returned invalid JSON: {e}", flush=True)
        return None
    return payload if isinstance(payload, dict) else None


def _parse_dictionary_changes(payload: dict) -> list[dict[str, object]]:
    changes = []
    for item in payload.get("changes") or []:
        if not isinstance(item, dict):
            continue
        changes.append({
            "from": str(item.get("from") or ""),
            "to": str(item.get("to") or ""),
            "count": int(item.get("count") or 0),
        })
    return changes


def _dictionary_runtime(text: str = "", base_prompt: str | None = None) -> DictionaryRuntimeResult:
    fallback = DictionaryRuntimeResult(text=text, prompt=_base_prompt_only(base_prompt))
    payload = _run_dictionary_helper_payload(text, base_prompt)
    if payload is None:
        return fallback

    error = payload.get("error")
    if error and STT_DEBUG:
        print(f"[dictionary] {error}", flush=True)

    return DictionaryRuntimeResult(
        text=str(payload.get("text", text)),
        prompt=payload.get("prompt") if isinstance(payload.get("prompt"), str) else None,
        terms=[str(term) for term in (payload.get("terms") or [])],
        changes=_parse_dictionary_changes(payload),
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


def _drop_hallucinated_segments(segment_list, audio_duration_s):
    """Split segments into (kept, dropped), dropping trailing-silence
    hallucinations before the text is assembled.

    A segment is dropped when the model itself flags it as very likely
    non-speech (``no_speech_prob`` high AND ``avg_logprob`` low together), or
    when its end timestamp runs past the captured audio (a hallucination beyond
    the recording, e.g. a 30 s "like and subscribe" tail on a 35 s clip).
    """
    kept = []
    dropped = []
    for segment in segment_list:
        no_speech = float(getattr(segment, "no_speech_prob", 0.0) or 0.0)
        avg_logprob = float(getattr(segment, "avg_logprob", 0.0) or 0.0)
        end = getattr(segment, "end", None)
        likely_silence = no_speech >= NO_SPEECH_DROP and avg_logprob <= NO_SPEECH_DROP_LOGPROB
        past_audio = (
            end is not None
            and audio_duration_s is not None
            and float(end) > audio_duration_s + SEGMENT_END_SLACK_S
        )
        (dropped if (likely_silence or past_audio) else kept).append(segment)
    return kept, dropped


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
    audio, capture_metrics = _boost_quiet_detail(raw_audio)
    dur = len(audio) / SR
    in_dbfs = 20 * np.log10(float(np.sqrt(np.mean(audio**2)) or 1e-9))
    use_context = CONTEXT_MIN_SECONDS > 0 and dur >= CONTEXT_MIN_SECONDS
    t0 = time.monotonic()
    dictionary_prompt = _dictionary_runtime("", INITIAL_PROMPT)
    prompt = dictionary_prompt.prompt
    # hallucination_silence_threshold only takes effect with word timestamps, so
    # enable both together when the guard is on.
    guard_kwargs = (
        {"word_timestamps": True, "hallucination_silence_threshold": HALLUCINATION_SILENCE_S}
        if HALLUCINATION_GUARD
        else {}
    )
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
        vad_parameters={
            "threshold": VAD_THRESHOLD,
            "min_silence_duration_ms": VAD_MIN_SILENCE_MS,
            "speech_pad_ms": VAD_SPEECH_PAD_MS,
        },
        **guard_kwargs,
    )
    segment_list = list(segments)
    segment_list, dropped_segments = _drop_hallucinated_segments(segment_list, dur)
    for segment in dropped_segments:
        print(
            f"[stt] dropped hallucinated segment: "
            f"no_speech={float(getattr(segment, 'no_speech_prob', 0.0) or 0.0):.2f} "
            f"end={float(getattr(segment, 'end', 0.0) or 0.0):.1f}s "
            f"text={getattr(segment, 'text', '')!r}",
            flush=True,
        )
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
        raw_dbfs=capture_metrics.raw_dbfs,
        peak=capture_metrics.peak,
        gain=capture_metrics.gain,
        noise_dbfs=capture_metrics.noise_dbfs,
        snr_db=capture_metrics.snr_db,
        input_status=capture_metrics.input_status,
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
