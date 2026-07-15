"""Pure summary/reporting helpers for the golden-benchmark corpus.

Split out of ``vp_benchmark`` so the result aggregation + the one-line UI summary
stay small, side-effect free and independently unit-testable. The UI surfaces
``format_summary_line`` verbatim in the runtime log, so this is the single place
the user-visible benchmark outcome is shaped.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Iterable

# Skip reason recorded when an item's audio is missing everywhere it was looked
# for. The summary counts these specifically so it can suggest where to record
# the corpus when ALL items were skipped for missing audio.
MISSING_AUDIO_REASON = "audio file missing"


def summarize_results(results: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """Collapse per-item benchmark events into one overall summary.

    Pure (no I/O) so it is unit-testable. Counts items, successes/failures and
    skips, and averages WER/CER over the *scored* (non-skipped, WER-bearing)
    items so a corpus with missing audio still yields a meaningful number.
    """
    rows = list(results)
    total = len(rows)
    passed = sum(1 for r in rows if r.get("benchmark_success"))
    skipped = sum(1 for r in rows if r.get("benchmark_skipped"))
    skipped_no_audio = sum(
        1 for r in rows
        if r.get("benchmark_skipped") and r.get("benchmark_error") == MISSING_AUDIO_REASON
    )
    failed = total - passed - skipped
    scored = [r for r in rows if not r.get("benchmark_skipped") and "wer" in r]
    avg_wer = sum(float(r["wer"]) for r in scored) / len(scored) if scored else None
    # Average CER over the rows that actually carry a `cer` field, not over every
    # scored (WER-bearing) row: a scored row can lack `cer`, and dividing by
    # `len(scored)` would understate the average. Mirrors `avg_wer` over its own
    # denominator; `None` when no scored row reports CER.
    cer_rows = [r for r in scored if "cer" in r]
    avg_cer = (
        sum(float(r["cer"]) for r in cer_rows) / len(cer_rows) if cer_rows else None
    )
    return {
        "total": total,
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "skipped_no_audio": skipped_no_audio,
        "scored": len(scored),
        "avg_wer": avg_wer,
        "avg_cer": avg_cer,
    }


def format_summary_line(
    summary: dict[str, Any],
    audio_hint_path: str | Path | None = None,
) -> str:
    """Render a one-line, human-readable summary prefixed with ``[benchmark]``.

    The UI surfaces this exact line in the runtime log so the user sees a concise
    pass count + overall WER without parsing the per-item JSONL.

    When EVERY item was skipped purely because its audio recording is missing
    (the common fresh-install case: corpus shipped, but no recordings yet) and an
    ``audio_hint_path`` is given, a "record corpus audio to <path>" hint is
    appended so the visible result is self-explanatory.
    """
    parts = [f"{summary['passed']}/{summary['total']} passed"]
    if summary["skipped"]:
        skipped = summary["skipped"]
        no_audio = summary.get("skipped_no_audio", 0)
        if no_audio == skipped:
            # Every skip was for missing audio — use the short, precise label.
            suffix = " (no audio)"
        elif no_audio:
            # Mixed: some skipped for other reasons — show the breakdown.
            suffix = f" ({no_audio} no audio)"
        else:
            suffix = ""
        parts.append(f"{skipped} skipped{suffix}")
    if summary["failed"]:
        parts.append(f"{summary['failed']} failed")
    if summary["avg_wer"] is not None:
        parts.append(f"avg WER {summary['avg_wer'] * 100:.1f}%")
    if summary["avg_cer"] is not None:
        parts.append(f"avg CER {summary['avg_cer'] * 100:.1f}%")
    line = "[benchmark] " + ", ".join(parts)
    total = summary["total"]
    if (
        audio_hint_path
        and total
        and summary.get("skipped_no_audio") == total
    ):
        line += f" — record corpus audio to {audio_hint_path}"
    return line
