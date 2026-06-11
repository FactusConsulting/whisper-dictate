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
    return flags


def format_health_line(metrics: dict[str, Any]) -> str:
    """Render the one-line ``[health]`` summary from a metrics dict.

    Expected keys (all optional; missing ones degrade gracefully):
      ``audio_raw_dbfs``, ``audio_snr_db``, ``audio_input_status``,
      ``audio_gain``, ``segments`` (list of dicts with ``avg_logprob``),
      ``post_mode``, ``post_processor``, ``no_text`` (truthy when the
      transcript was empty/dropped).
    """
    avg_logprob = _mean_avg_logprob(metrics.get("segments"))
    band = _confidence_band(avg_logprob)
    if avg_logprob is None:
        confidence = f"confidence {band}"
    else:
        confidence = f"confidence {band} ({avg_logprob:.2f})"
    parts = [_mic_segment(metrics), confidence, _post_segment(metrics)]
    parts.extend(_warn_flags(metrics, band))
    return "[health] " + " | ".join(parts)
