"""Per-utterance ``[health]`` line — a concise, plain-language signal for the
"Basic" diagnostics level.

The whole job of this module is one PURE function, :func:`format_health_line`,
over a metrics dict (the same field names the ``[utterance]`` event carries), so
it is trivially unit-testable without any audio/model state. The dictation loop
assembles the dict and prints the returned string with a plain ``print(...,
flush=True)`` just like the other ``[tag]`` lines.

Format (single line, ASCII-safe):

    [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq

with terse ``| WARN ...`` flags appended only when something looks off.
"""
from __future__ import annotations

from typing import Any, Sequence

# Confidence bands over the aggregate avg_logprob. We use the MEAN of the
# segments' avg_logprob (not the min): a single low-confidence trailing segment
# on otherwise-clean speech should not drag the whole utterance into "low" — the
# real hallucination tails are already dropped upstream by
# _drop_hallucinated_segments before they reach here, so the mean is the honest
# "how sure was the model overall" read the user asked for.
CONFIDENCE_HIGH = -0.35  # avg_logprob >= this -> "high"
CONFIDENCE_OK = -0.60    # avg_logprob in [OK, HIGH) -> "ok"; below OK -> "low"

# Graduated health grade thresholds (signal-to-noise ratio, in dB). The grade is
# a single 4-level quality verdict for the whole utterance — "how well did this
# dictation go" — folding the mic input status, the model's confidence band, the
# post-processing outcome and the SNR into one of "perfect"/"good"/"fair"/"poor".
# The Rust UI maps that stable token to a colour/icon/label; see health_grade.
HEALTH_SNR_POOR = 6.0      # snr < this is "poor" (matches the WARN quiet-input bar)
HEALTH_SNR_GOOD = 20.0     # "good" needs snr >= this (clear speech contrast)
HEALTH_SNR_PERFECT = 30.0  # "perfect" needs snr >= this (pristine input)

# Mic input-status buckets (the exact tokens vp_audio._input_level_status emits:
# too_quiet / low_snr / clip_risk / hot / quiet / good). A status in
# _INPUT_STATUS_POOR means the capture itself is unusable for this utterance.
_INPUT_STATUS_POOR = frozenset({"too_quiet", "low_snr", "clip_risk"})

# The four grade tokens, worst -> best, emitted verbatim in the `[health]` line.
GRADE_POOR = "poor"
GRADE_FAIR = "fair"
GRADE_GOOD = "good"
GRADE_PERFECT = "perfect"

_REMOTE_STT_WITHOUT_CONFIDENCE = frozenset({"openai"})


def _mean_avg_logprob(segments: Sequence[dict[str, Any]] | None) -> float | None:
    """Mean ``avg_logprob`` across segments, or ``None`` when unavailable."""
    if not segments:
        return None
    values = [
        float(seg["avg_logprob"])
        for seg in segments
        if isinstance(seg, dict) and seg.get("avg_logprob") is not None
    ]
    if not values:
        return None
    return sum(values) / len(values)


def _confidence_band(avg_logprob: float | None) -> str:
    if avg_logprob is None:
        return "n/a"
    if avg_logprob >= CONFIDENCE_HIGH:
        return "high"
    if avg_logprob >= CONFIDENCE_OK:
        return "ok"
    return "low"


def _snr_db(metrics: dict[str, Any]) -> float | None:
    """The utterance SNR in dB, or ``None`` when unavailable/unparsable."""
    try:
        return float(metrics.get("audio_snr_db"))
    except (TypeError, ValueError):
        return None


def _confidence_n_a_is_expected(metrics: dict[str, Any]) -> bool:
    """Remote OpenAI-compatible STT does not expose segment logprobs."""
    backend = str(metrics.get("stt_backend") or "").strip().casefold()
    return backend in _REMOTE_STT_WITHOUT_CONFIDENCE


def health_grade(metrics: dict[str, Any]) -> str:
    """Fold every per-utterance signal into one 4-level quality grade.

    Returns one of :data:`GRADE_PERFECT` / :data:`GRADE_GOOD` /
    :data:`GRADE_FAIR` / :data:`GRADE_POOR` ("perfect"/"good"/"fair"/"poor").
    Pure and total: any missing or unparsable signal degrades gracefully toward
    the safe middle "fair" grade rather than raising — a metrics dict is never
    trusted to be complete (the no_text path, for example, carries no segments).

    Priority — the worst signal wins:

    * **poor**  — the input itself is unusable: ``audio_input_status`` in
      {too_quiet, low_snr, clip_risk}, OR the model's confidence band is "low",
      OR ``audio_snr_db`` is below :data:`HEALTH_SNR_POOR`.
    * **fair**  — not poor, but something is off: confidence band "ok",
      post-processing fell back to raw text, the input ran "hot", OR a signal we
      need to judge quality is missing. OpenAI-compatible remote STT is the
      exception: it does not expose confidence, so clean audio may still be
      "good" but never "perfect".
    * **good**  — clean: confidence band "high" (or expected-unavailable for
      remote STT) and SNR >= :data:`HEALTH_SNR_GOOD`. A "quiet" input is fine
      here (the worker boosts it) as long as the other signals hold up.
    * **perfect** — pristine: confidence "high", no post-processing fallback, the
      input status is exactly "good", and SNR >= :data:`HEALTH_SNR_PERFECT`.
    """
    band = _confidence_band(_mean_avg_logprob(metrics.get("segments")))
    status = str(metrics.get("audio_input_status") or "").strip()
    snr = _snr_db(metrics)
    post_fallback = bool(metrics.get("post_fallback"))
    confidence_n_a_is_neutral = (
        band == "n/a"
        and not metrics.get("no_text")
        and _confidence_n_a_is_expected(metrics)
    )

    # poor: any single unusable signal drags the whole utterance down.
    if (
        status in _INPUT_STATUS_POOR
        or band == "low"
        or (snr is not None and snr < HEALTH_SNR_POOR)
    ):
        return GRADE_POOR

    # fair: not poor, but a known degradation OR a missing signal we'd need to
    # promote it. We never claim "good"/"perfect" on incomplete information —
    # a missing/empty ``audio_input_status`` is treated like a missing SNR
    # (Codex P3 on PR #342: an empty status used to silently promote partial
    # payloads such as ``{segments: [...], audio_snr_db: 42}`` to "good").
    if (
        band == "ok"
        or (band == "n/a" and not confidence_n_a_is_neutral)
        or post_fallback
        or status == "hot"
        or not status
        or snr is None
    ):
        return GRADE_FAIR

    # From here band is high, or n/a because the remote STT backend does not
    # expose confidence, and snr is a real number >= HEALTH_SNR_POOR.
    if (
        band == "high"
        and not post_fallback
        and status == "good"
        and snr >= HEALTH_SNR_PERFECT
    ):
        return GRADE_PERFECT

    if snr >= HEALTH_SNR_GOOD:
        return GRADE_GOOD

    # band == "high" but SNR sits between POOR and GOOD — honestly only "fair".
    return GRADE_FAIR


def _round_int(value: Any) -> int | None:
    try:
        return int(round(float(value)))
    except (TypeError, ValueError):
        return None


def _mic_segment(metrics: dict[str, Any]) -> str:
    raw = _round_int(metrics.get("audio_raw_dbfs"))
    snr = _round_int(metrics.get("audio_snr_db"))
    status = str(metrics.get("audio_input_status") or "").strip() or "n/a"
    raw_s = f"{raw}dBFS" if raw is not None else "?dBFS"
    snr_s = f"SNR {snr}dB" if snr is not None else "SNR ?dB"
    # When the input was quiet, the worker boosted it — surface the applied gain
    # so the user can see how hard we had to work, e.g. "quiet (boosted 11x)".
    if status == "quiet":
        gain = metrics.get("audio_gain")
        try:
            gain_f = float(gain)
        except (TypeError, ValueError):
            gain_f = None
        if gain_f is not None and gain_f > 1.0:
            status = f"quiet (boosted {gain_f:.0f}x)"
    return f"mic {raw_s} {snr_s} {status}"


def _post_segment(metrics: dict[str, Any]) -> str:
    mode = str(metrics.get("post_mode") or "").strip()
    processor = str(metrics.get("post_processor") or "").strip()
    # No real post-processing -> "off" (matches the UI's "Post-processing off").
    if not processor or processor == "none" or not mode or mode == "raw":
        return "post off"
    return f"post {mode}/{processor}"


def _warn_flags(metrics: dict[str, Any], band: str) -> list[str]:
    """The terse 'are we off?' flags, only the triggered ones."""
    flags: list[str] = []
    if band == "low":
        flags.append("WARN low confidence")
    # Quiet-but-clean is fine (the user's own setup is quiet+clean and works), so
    # only warn when the input is quiet AND its speech contrast (SNR) is also low.
    status = str(metrics.get("audio_input_status") or "").strip()
    snr = metrics.get("audio_snr_db")
    try:
        snr_f = float(snr)
    except (TypeError, ValueError):
        snr_f = None
    quiet = status in ("quiet", "too_quiet")
    low_snr = snr_f is not None and snr_f < 6.0
    if quiet and low_snr:
        flags.append("WARN quiet input")
    if metrics.get("no_text"):
        flags.append("WARN no text")
    # Post-processing silently fell back to raw, uncleaned text (most often a
    # timeout). Surface it as a WARN segment so the Rust health card turns amber
    # and the user knows the rewrite did not run. ASCII-safe (no arrow glyph).
    if metrics.get("post_fallback"):
        latency = _round_int(metrics.get("post_latency_ms", 0)) or 0
        secs = max(0, latency // 1000)
        flags.append(f"WARN post timeout->raw ({secs}s)")
    return flags


def format_health_line(metrics: dict[str, Any]) -> str:
    """Render the one-line ``[health]`` summary from a metrics dict.

    Expected keys (all optional; missing ones degrade gracefully):
      ``audio_raw_dbfs``, ``audio_snr_db``, ``audio_input_status``,
      ``audio_gain``, ``segments`` (list of dicts with ``avg_logprob``),
      ``post_mode``, ``post_processor``, ``post_fallback`` (truthy when
      post-processing fell back to raw text, e.g. on timeout), with optional
      ``post_latency_ms`` for the elapsed wall-clock, ``no_text`` (truthy when
      the transcript was empty/dropped).
    """
    avg_logprob = _mean_avg_logprob(metrics.get("segments"))
    band = _confidence_band(avg_logprob)
    if avg_logprob is None:
        confidence = f"confidence {band}"
    else:
        confidence = f"confidence {band} ({avg_logprob:.2f})"
    parts = [_mic_segment(metrics), confidence, _post_segment(metrics)]
    parts.extend(_warn_flags(metrics, band))
    # The graded verdict is always the LAST segment, e.g. " | grade=good". It is
    # a stable token the Rust UI maps to a colour/icon/label. It must never start
    # with "WARN" so the existing structural has_warning detection (which keys off
    # a `| WARN ...` segment) is unaffected by it.
    parts.append(f"grade={health_grade(metrics)}")
    return "[health] " + " | ".join(parts)
