"""Suggest dictionary replacements from benchmark/history JSONL."""
from __future__ import annotations

import difflib
import json
import re
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

from vp_dictionary import DICTIONARY


@dataclass
class ReplacementSuggestion:
    source: str
    target: str
    count: int = 0
    confidence: float = 0.0
    reason: str = ""
    samples: list[str] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return {
            "from": self.source,
            "to": self.target,
            "count": self.count,
            "confidence": round(self.confidence, 3),
            "reason": self.reason,
            "samples": self.samples[:5],
        }


def _words(text: str) -> list[str]:
    return re.findall(r"[\wæøåÆØÅ.-]+", text, flags=re.UNICODE)


def _normalize(text: str) -> str:
    return " ".join(_words(text)).casefold()


def _ngrams(words: list[str], size: int) -> Iterable[str]:
    if size <= 0 or len(words) < size:
        return
    for i in range(0, len(words) - size + 1):
        yield " ".join(words[i:i + size])


def _add_suggestion(
    source: str,
    target: str,
    *,
    confidence: float,
    reason: str,
    sample: str,
    existing: set[tuple[str, str]],
    counts: dict[tuple[str, str], int],
    best: dict[tuple[str, str], float],
    samples: dict[tuple[str, str], list[str]],
    reasons: dict[tuple[str, str], str],
) -> None:
    source = source.strip()
    target = target.strip()
    source_norm = _normalize(source)
    target_norm = _normalize(target)
    if not source_norm or not target_norm or source_norm == target_norm:
        return
    if (source_norm, target_norm) in existing:
        return
    key = (source, target)
    counts[key] += 1
    best[key] = max(best[key], confidence)
    reasons[key] = reason
    if sample and sample not in samples[key]:
        samples[key].append(sample)


def _similarity(left: str, right: str) -> float:
    return difflib.SequenceMatcher(None, _normalize(left), _normalize(right)).ratio()


def _read_jsonl(path: str | Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with Path(path).expanduser().open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                rows.append(obj)
    return rows


def _known_targets(rows: Iterable[dict[str, Any]]) -> list[str]:
    targets: list[str] = []
    seen: set[str] = set()
    for term in DICTIONARY.terms:
        key = _normalize(term)
        if key and key not in seen:
            seen.add(key)
            targets.append(term)
    for row in rows:
        for key in ("reference_terms", "term_misses", "term_hits"):
            values = row.get(key) or []
            if isinstance(values, str):
                values = [values]
            for value in values:
                term = str(value).strip()
                norm = _normalize(term)
                if norm and norm not in seen:
                    seen.add(norm)
                    targets.append(term)
    return targets


def _existing_replacement_pairs() -> set[tuple[str, str]]:
    return {(_normalize(src), _normalize(dst)) for src, dst in DICTIONARY.replacements.items()}


def suggest_replacements_from_rows(
    rows: list[dict[str, Any]],
    *,
    min_confidence: float = 0.62,
) -> list[ReplacementSuggestion]:
    targets = _known_targets(rows)
    existing = _existing_replacement_pairs()
    grouped: dict[tuple[str, str], ReplacementSuggestion] = {}
    counts: dict[tuple[str, str], int] = defaultdict(int)
    best: dict[tuple[str, str], float] = defaultdict(float)
    samples: dict[tuple[str, str], list[str]] = defaultdict(list)
    reasons: dict[tuple[str, str], str] = {}

    for row in rows:
        text = str(row.get("text") or row.get("dictionary_text") or row.get("raw_text") or "")
        if not text:
            continue
        sample = str(row.get("corpus_id") or row.get("target_title") or row.get("source_file") or "")
        missed_terms = [str(term).strip() for term in (row.get("term_misses") or [])
                        if isinstance(term, str) and str(term).strip()]
        has_reference_context = any(row.get(key) for key in (
            "reference_text", "reference_terms", "term_misses", "term_hits",
            "wer", "cer",
        ))
        row_targets = []
        row_targets.extend(missed_terms)
        if not row_targets and not has_reference_context:
            row_targets.extend(targets)
        words = _words(text)
        reference_words = _words(str(row.get("reference_text") or ""))
        for term in missed_terms:
            term_words = _words(term)
            if not term_words:
                continue
            size = len(term_words)
            for i in range(0, len(reference_words) - size + 1):
                if _normalize(" ".join(reference_words[i:i + size])) != _normalize(term):
                    continue
                for candidate_size in sorted({size, size + 1}):
                    if i + candidate_size <= len(words):
                        source = " ".join(words[i:i + candidate_size])
                        _add_suggestion(
                            source,
                            term,
                            confidence=max(0.70, _similarity(source, term)),
                            reason="term_miss_position_match",
                            sample=sample,
                            existing=existing,
                            counts=counts,
                            best=best,
                            samples=samples,
                            reasons=reasons,
                        )
        for target in row_targets:
            if not target:
                continue
            target_norm = _normalize(target)
            if not target_norm or target_norm in _normalize(text):
                continue
            target_len = max(1, len(_words(target)))
            candidate_sizes = sorted({target_len - 1, target_len, target_len + 1})
            for size in candidate_sizes:
                if size <= 0:
                    continue
                for source in _ngrams(words, size) or []:
                    source_norm = _normalize(source)
                    if source_norm == target_norm:
                        continue
                    if (source_norm, target_norm) in existing:
                        continue
                    confidence = _similarity(source, target)
                    if confidence < min_confidence:
                        continue
                    _add_suggestion(
                        source,
                        target,
                        confidence=confidence,
                        reason="term_miss_fuzzy_match" if target in missed_terms else "dictionary_fuzzy_match",
                        sample=sample,
                        existing=existing,
                        counts=counts,
                        best=best,
                        samples=samples,
                        reasons=reasons,
                    )

    for (source, target), count in counts.items():
        grouped[(source, target)] = ReplacementSuggestion(
            source=source,
            target=target,
            count=count,
            confidence=best[(source, target)],
            reason=reasons[(source, target)],
            samples=samples[(source, target)][:5],
        )
    return sorted(grouped.values(), key=lambda s: (-s.count, -s.confidence, s.target.casefold()))


def suggest_replacements(path: str | Path, *, min_confidence: float = 0.62) -> list[ReplacementSuggestion]:
    return suggest_replacements_from_rows(_read_jsonl(path), min_confidence=min_confidence)


def print_suggestions(
    suggestions: list[ReplacementSuggestion],
    *,
    as_json: bool = False,
) -> None:
    if as_json:
        print(json.dumps([s.as_dict() for s in suggestions], ensure_ascii=False, sort_keys=True), flush=True)
        return
    if not suggestions:
        print("No dictionary replacement suggestions found.", flush=True)
        return
    for s in suggestions:
        samples = f" samples={', '.join(s.samples[:3])}" if s.samples else ""
        print(
            f"{s.source!r} -> {s.target!r}  "
            f"count={s.count} confidence={s.confidence:.2f} reason={s.reason}{samples}",
            flush=True,
        )
