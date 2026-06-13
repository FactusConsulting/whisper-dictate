"""Whisper transcription core — pure function plus hallucination filter.

Imports faster_whisper lazily inside _transcribe so the module is cheap to
import while the runtime module keeps the heavy DLL/CUDA bootstrap centralized.
"""
from __future__ import annotations

import os
import json
import re
import subprocess
import threading
import time
from dataclasses import dataclass, field
from importlib import resources
from typing import Any

import numpy as np

from whisper_dictate.vp_audio import _boost_quiet, _boost_quiet_detail, _looks_like_speech
from whisper_dictate.vp_config import apply_config_to_environ, get_value

apply_config_to_environ()

SR = 16000

# Module-level lock that SERIALIZES every faster-whisper model.transcribe()
# call — both the final transcription at key release (_transcribe_detail) and
# the in-flight live previews (vp_preview). faster-whisper / CTranslate2 model
# objects are not safe to drive from two threads at once, so the preview thread
# and the final pass must never call transcribe concurrently. The final pass
# acquires it blocking (it MAY wait for at most one in-flight preview chunk —
# bounded, since a preview only transcribes the buffer captured so far); the
# preview path acquires it non-blocking so previews never queue up behind each
# other or behind the final pass.
TRANSCRIBE_LOCK = threading.Lock()

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
# Speech-rate plausibility gate: a transcript whose chars-per-second exceeds
# this is humanly impossible (real speech is 15-25 chars/s) and is almost
# certainly a hallucinated credit/caption on quiet input. "0" disables the gate.
# Live-reloadable (mirrored from config in Dictate._apply_runtime_module_config).
MAX_CHARS_PER_SECOND = float(get_value("VOICEPI_MAX_CHARS_PER_SECOND", "30") or "30")
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

# Anti-hallucination PATTERN DATA lives in data/hallucination_patterns.json (see
# that file's _comment for the full design); this module owns only the matching
# LOGIC. The loader below rebuilds, byte-for-byte, the same HALLUCINATIONS
# frozenset and anchored _CREDIT_RE the inline literals used to produce: exact
# whole-text blacklist, plus a credit regex whose phrase prefixes require a
# trailing year (so real dictation starting with them survives) and whose bare
# company names match with an optional year.
_PATTERNS_RESOURCE = "hallucination_patterns.json"


def _load_hallucination_patterns() -> dict:
    """Read the pattern-data JSON once at import via importlib.resources.

    importlib.resources (not a filesystem path) stays robust across the
    zip/installed layouts the app ships in.
    """
    raw = (
        resources.files("whisper_dictate.data")
        .joinpath(_PATTERNS_RESOURCE)
        .read_text(encoding="utf-8")
    )
    return json.loads(raw)


def _build_credit_re(patterns: dict) -> re.Pattern[str]:
    """Compile the anchored subtitle-credit regex from the pattern data.

    The shared year tail is appended ONCE to the whole credit_phrase_prefixes
    alternation; each bare_company_names entry is an independent branch carrying
    its own optional year suffix. Anchored both ends, trailing punctuation/space
    tolerated.
    """
    phrase_group = "|".join(patterns["credit_phrase_prefixes"])
    body = "(?:" + "|".join([
        f"(?:{phrase_group}){patterns['credit_phrase_year_tail']}",
        *patterns["bare_company_names"],
    ]) + ")"
    return re.compile(rf"^{body}[\s.!?]*$", re.IGNORECASE)


def _build_credit_prefix_re(patterns: dict) -> re.Pattern[str]:
    """Compile a START-anchored regex of the credit-phrase prefixes WITHOUT the
    year requirement — a match means the text merely BEGINS like a subtitle
    credit ("Danske tekster af …"). This is intentionally loose: it also matches
    real dictation such as "danske tekster af høj kvalitet", so callers MUST pair
    it with a second, independent signal (an impossible speech rate) before
    dropping anything — see ``_drop_hallucinated_segments``. It exists to catch
    year-LESS credit hallucinations that ``_CREDIT_RE`` deliberately won't match
    on text alone (e.g. "Danske tekster af Nicolai Winther").
    """
    phrase_group = "|".join(patterns["credit_phrase_prefixes"])
    return re.compile(rf"^(?:{phrase_group})", re.IGNORECASE)


_HALLUCINATION_PATTERNS = _load_hallucination_patterns()
HALLUCINATIONS: frozenset[str] = frozenset(_HALLUCINATION_PATTERNS["exact_blacklist"])
_CREDIT_RE = _build_credit_re(_HALLUCINATION_PATTERNS)
_CREDIT_PREFIX_RE = _build_credit_prefix_re(_HALLUCINATION_PATTERNS)


def _looks_like_credit(text: str) -> bool:
    """True when the WHOLE text is a subtitle/caption credit (anchored match)."""
    return bool(_CREDIT_RE.match(text.strip().lower()))


def _looks_like_credit_prefix(text: str) -> bool:
    """True when the text BEGINS with a subtitle-credit phrase (year-less, loose).

    Only safe to act on together with an impossible-rate signal; on its own it
    matches real dictation that happens to start with these phrases.
    """
    return bool(_CREDIT_PREFIX_RE.match(text.strip().lower()))


def is_hallucination(text: str) -> bool:
    return text.lower().rstrip() in HALLUCINATIONS or _looks_like_credit(text)


def _drop_hallucinated_segments(segment_list, audio_duration_s):
    """Split segments into (kept, dropped), dropping trailing-silence
    hallucinations before the text is assembled.

    A segment is dropped when:

    * the model itself flags it as very likely non-speech (``no_speech_prob``
      high AND ``avg_logprob`` low together); or
    * its end timestamp runs past the captured audio (a hallucination beyond the
      recording, e.g. a 30 s "like and subscribe" tail on a 35 s clip); or
    * its OWN text is a known subtitle/caption credit (``is_hallucination`` —
      the exact-match blacklist plus the year-anchored credit regex) AND the
      model corroborates non-speech via a high ``no_speech_prob``
      (``>= NO_SPEECH_DROP``). This catches a credit emitted as a SEPARATE
      segment appended to a long real utterance — the whole assembled text is
      then not a credit, so the whole-text ``is_hallucination`` check downstream
      never sees it. The silence gate is required: the credit regex is loose
      enough (year-anchored, up to 60 leading chars) to match confident real
      dictation like "oversat af Google i 2023", so text shape alone must never
      drop a segment; or
    * its text is a known credit OR merely BEGINS with a credit prefix
      ("Danske tekster af …") AND its OWN char-rate is humanly impossible (e.g.
      the 60-char subtitle credit on a 0.30 s tail = ~200 chars/s). This needs NO
      ``no_speech_prob`` corroboration: the two independent signals (credit shape
      AND impossible speech rate) never co-occur on real speech, so it stays
      safe. It catches both the moderate-``no_speech`` repro the ``credit_pattern``
      gate misses (observed 0.43 < ``NO_SPEECH_DROP``) AND year-LESS credits the
      year-anchored ``is_hallucination`` regex deliberately won't match on text
      alone (e.g. "Danske tekster af Nicolai Winther" — the prefix matches real
      dictation too, hence the impossible-rate pairing).

    Returns (kept, dropped). ``dropped`` items are tagged with a transient
    ``_drop_reason`` attribute for diagnostics.
    """
    kept = []
    dropped = []
    for segment in segment_list:
        no_speech = float(getattr(segment, "no_speech_prob", 0.0) or 0.0)
        avg_logprob = float(getattr(segment, "avg_logprob", 0.0) or 0.0)
        start = getattr(segment, "start", None)
        end = getattr(segment, "end", None)
        seg_text = getattr(segment, "text", "") or ""
        likely_silence = no_speech >= NO_SPEECH_DROP and avg_logprob <= NO_SPEECH_DROP_LOGPROB
        past_audio = (
            end is not None
            and audio_duration_s is not None
            and float(end) > audio_duration_s + SEGMENT_END_SLACK_S
        )
        is_credit = is_hallucination(seg_text)
        # A credit (or a year-LESS subtitle-credit PREFIX like "Danske tekster
        # af …") whose OWN char-rate is humanly impossible is a hallucination
        # needing NO no_speech signal — two independent signals (credit shape AND
        # impossible rate) never co-occur on real speech. Catches BOTH the
        # moderate-no_speech repro the credit_pattern gate (no_speech >=
        # NO_SPEECH_DROP) lets through, AND year-less credits that is_hallucination
        # deliberately won't match on text alone (the prefix also matches real
        # dictation, so it is ONLY ever dropped together with impossible rate).
        seg_duration = (
            float(end) - float(start)
            if end is not None and start is not None
            else None
        )
        credit_rate = (
            (is_credit or _looks_like_credit_prefix(seg_text))
            and seg_duration is not None
            and _speech_rate_exceeded(seg_text, seg_duration)
        )
        reason = None
        if likely_silence:
            reason = "no_speech+logprob"
        elif past_audio:
            reason = "end_past_audio"
        elif is_credit and no_speech >= NO_SPEECH_DROP:
            reason = "credit_pattern"
        elif credit_rate:
            reason = "credit_rate"
        if reason is not None:
            try:
                segment._drop_reason = reason
            except (AttributeError, TypeError):
                pass
            dropped.append(segment)
        else:
            kept.append(segment)
    return kept, dropped


def _speech_rate_exceeded(text: str, duration_s: float) -> bool:
    """Pure predicate: True when ``text`` packs more chars/second than humanly
    plausible.

    Real speech runs ~15-25 chars/s; the default 30 cap leaves headroom. The
    classic credit hallucination (60 chars from a 0.31 s tap = ~193 chars/s) is
    far above any real rate. ``MAX_CHARS_PER_SECOND`` of 0 disables the gate.
    Side-effect-free, so it is safe to call per-segment inside
    ``_drop_hallucinated_segments``; ``_exceeds_speech_rate`` wraps it with the
    diagnostic log line for the whole-text path.
    """
    if MAX_CHARS_PER_SECOND <= 0:
        return False
    return len(text) / max(duration_s, 0.1) > MAX_CHARS_PER_SECOND


def _exceeds_speech_rate(text: str, duration_s: float) -> bool:
    """``_speech_rate_exceeded`` plus a diagnostic log line when it fires."""
    if not _speech_rate_exceeded(text, duration_s):
        return False
    chars = len(text)
    rate = chars / max(duration_s, 0.1)
    print(
        f"[stt] dropped: {chars} chars in {duration_s:.1f}s = "
        f"{rate:.0f} chars/s > {MAX_CHARS_PER_SECOND:.0f} (hallucination guard)",
        flush=True,
    )
    return True


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
    # Hold TRANSCRIBE_LOCK across the whole decode (the generator is lazy, so
    # segments must be drained inside the lock too). This serializes the final
    # pass with any in-flight live preview — the final pass may wait for at most
    # one bounded preview chunk to finish, which is acceptable.
    with TRANSCRIBE_LOCK:
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
            f"reason={getattr(segment, '_drop_reason', '?')} "
            f"no_speech={float(getattr(segment, 'no_speech_prob', 0.0) or 0.0):.2f} "
            f"logprob={float(getattr(segment, 'avg_logprob', 0.0) or 0.0):.2f} "
            f"end={float(getattr(segment, 'end', 0.0) or 0.0):.1f}s "
            f"text={getattr(segment, 'text', '')!r}",
            flush=True,
        )
    # Concatenate with Whisper's OWN spacing. Each segment text already
    # carries a leading space on word boundaries (BPE tokens); strip()+
    # " ".join() drops that at segment joins -> "hørerdig". Join raw,
    # then collapse whitespace runs to one space.
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segment_list)).strip()
    if raw_text and _exceeds_speech_rate(raw_text, dur):
        # Humanly impossible char rate -> hallucinated credit/caption. Drop the
        # text (and its segments); downstream this surfaces as an empty result
        # (reason="empty" -> state=no_text), not as an is_hallucination match.
        raw_text = ""
        segment_list = []
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


def transcribe_preview(model, pcm: np.ndarray, lang: str | None) -> str:
    """Fast, display-only transcription of an in-progress recording buffer.

    Used by the live-preview thread (vp_preview) to show the sentence growing
    while the user is still holding the key. It is deliberately CHEAP and never
    touches the quality knobs the final pass uses:

      * beam_size=1, temperature=0.0, condition_on_previous_text=False — greedy,
        no fallback ladder, no cross-segment context, so it is as fast as
        possible and never queues.
      * the existing VAD settings are reused (read-only) so silence handling
        matches the final pass closely enough for a preview.
      * the hallucination guard / word-timestamps are skipped (extra compute we
        don't want for a throwaway preview).

    This must NOT mutate any shared decode state (it only reads module globals)
    and the final pass is the single source of truth for the injected text.

    Returns the (whitespace-collapsed) preview text, or "" on no/empty speech.
    The caller is responsible for acquiring TRANSCRIBE_LOCK (non-blocking) so a
    preview never runs concurrently with the final pass or another preview.
    """
    audio = pcm.reshape(-1).astype(np.float32) / 32768.0
    segments, _info = model.transcribe(
        audio,
        language=lang,
        beam_size=1,
        temperature=0.0,
        condition_on_previous_text=False,
        no_speech_threshold=0.45,
        log_prob_threshold=-1.0,
        vad_filter=True,
        vad_parameters={
            "threshold": VAD_THRESHOLD,
            "min_silence_duration_ms": VAD_MIN_SILENCE_MS,
            "speech_pad_ms": VAD_SPEECH_PAD_MS,
        },
    )
    raw_text = re.sub(r"\s+", " ", "".join(s.text for s in segments)).strip()
    return raw_text
