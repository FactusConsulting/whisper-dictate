"""Suggest dictionary replacements from benchmark/history JSONL."""
from __future__ import annotations

import difflib
import json
import os
import re
import subprocess
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

_COMMON_SOURCE_WORDS = {
    "a", "an", "and", "as", "at", "be", "but", "by", "for", "from", "i",
    "in", "is", "it", "le", "of", "og", "on", "or", "skal", "the", "til",
    "to", "with", "de", "den", "det", "der", "du", "en", "et", "jeg",
    "kan", "med", "mig", "på", "så", "vi", "eller", "set", "fra", "type",
}
_COMMON_SOURCE_PHRASES = {
    "begge", "begge forstå", "code", "code i", "consulting", "day", "kode",
    "large", "le code", "le terminal", "terminal", "two", "whisper",
    "claude", "consulting 2d", "contre celui", "dæv eller",
    "eller brød", "faktus consulting 2d", "faktus consulting og",
    "kom", "kobberites klosteret", "kodex versus", "køre", "køre klosteret",
    "large-v3 and", "mcp", "mcp rac", "pisit backend",
    "que", "serveren til remote lokal postprocessing",
    "serveren til remote lokal postprocessing.", "sit",
    "signal-to-noise-ratio tydelig i terminalen",
    "signal-to-noise-ratio tydelig i terminalen.", "typ", "voice pisit",
    "ændringen pudst", "ændringen pudste",
}
_SHORT_SOURCE_ALLOWLIST = {
    "2d", "dbfs", "qn", "rac", "rag", "snr", "stt", "ui", "vad", "vlm",
    "xkb",
}


@dataclass
class DictionarySnapshot:
    terms: list[str] = field(default_factory=list)
    replacements: dict[str, str] = field(default_factory=dict)


def _parse_replacements(payload: dict) -> dict[str, str]:
    replacements: dict[str, str] = {}
    for item in payload.get("replacements") or []:
        if isinstance(item, dict):
            source = str(item.get("from") or "").strip()
            target = str(item.get("to") or "").strip()
            if source and target:
                replacements[source] = target
    return replacements


def _load_dictionary_snapshot() -> DictionarySnapshot:
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        return DictionarySnapshot()
    try:
        r = subprocess.run(
            [helper, "dictionary-runtime"],
            input=json.dumps({"base_prompt": None, "text": ""}, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=5,
            shell=False,
        )
        if r.returncode != 0:
            return DictionarySnapshot()
        payload = json.loads(r.stdout or "{}")
    except Exception:  # noqa: BLE001 - suggestions can run without live dictionary
        return DictionarySnapshot()
    if not isinstance(payload, dict):
        return DictionarySnapshot()
    return DictionarySnapshot(
        terms=[str(term) for term in (payload.get("all_terms") or payload.get("terms") or [])],
        replacements=_parse_replacements(payload),
    )


DICTIONARY = _load_dictionary_snapshot()


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


@dataclass
class _SuggestionState:
    existing: set[tuple[str, str]]
    counts: dict[tuple[str, str], int] = field(default_factory=lambda: defaultdict(int))
    best: dict[tuple[str, str], float] = field(default_factory=lambda: defaultdict(float))
    samples: dict[tuple[str, str], list[str]] = field(default_factory=lambda: defaultdict(list))
    reasons: dict[tuple[str, str], str] = field(default_factory=dict)


def _words(text: str) -> list[str]:
    return re.findall(r"[\wæøåÆØÅ.-]+", text, flags=re.UNICODE)


def _normalize(text: str) -> str:
    return " ".join(_words(text)).casefold()


def _is_risky_source(source: str) -> bool:
    normalized = _normalize(source)
    if not normalized:
        return True
    words = normalized.split()
    if normalized in _COMMON_SOURCE_PHRASES:
        return True
    if len(words) == 1:
        word = words[0]
        return (
            word in _COMMON_SOURCE_WORDS
            or (len(word) <= 2 and word not in _SHORT_SOURCE_ALLOWLIST)
        )
    if words[0] in _COMMON_SOURCE_WORDS or words[-1] in _COMMON_SOURCE_WORDS:
        return True
    if len(words) <= 3 and any(word in _COMMON_SOURCE_WORDS for word in words):
        return True
    return False


def _known_source_terms() -> set[str]:
    return {_normalize(term) for term in DICTIONARY.terms if _normalize(term)}


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
    state: _SuggestionState,
) -> None:
    source = source.strip()
    target = target.strip()
    source_norm = _normalize(source)
    target_norm = _normalize(target)
    if not source_norm or not target_norm or source_norm == target_norm:
        return
    if _is_risky_source(source):
        return
    if source_norm in _known_source_terms():
        return
    if (source_norm, target_norm) in state.existing:
        return
    key = (source, target)
    state.counts[key] += 1
    state.best[key] = max(state.best[key], confidence)
    state.reasons[key] = reason
    if sample and sample not in state.samples[key]:
        state.samples[key].append(sample)


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
    seen: set[str] = set()
    targets: list[str] = []
    for term in _dictionary_terms() + _row_reference_terms(rows):
        norm = _normalize(term)
        if norm and norm not in seen:
            seen.add(norm)
            targets.append(term)
    return targets


def _dictionary_terms() -> list[str]:
    return [str(term) for term in DICTIONARY.terms]


def _row_reference_terms(rows: Iterable[dict[str, Any]]) -> list[str]:
    terms: list[str] = []
    for row in rows:
        for value in _row_term_values(row):
            term = str(value).strip()
            if term:
                terms.append(term)
    return terms


def _row_term_values(row: dict[str, Any]) -> Iterable[Any]:
    for key in ("reference_terms", "term_misses", "term_hits"):
        values = row.get(key) or []
        if isinstance(values, str):
            yield values
        else:
            yield from values


def _existing_replacement_pairs() -> set[tuple[str, str]]:
    return {(_normalize(src), _normalize(dst)) for src, dst in DICTIONARY.replacements.items()}


def _missed_terms(row: dict[str, Any]) -> list[str]:
    return [
        str(term).strip()
        for term in (row.get("term_misses") or [])
        if isinstance(term, str) and str(term).strip()
    ]


def _has_reference_context(row: dict[str, Any]) -> bool:
    return any(row.get(key) for key in (
        "reference_text", "reference_terms", "term_misses", "term_hits", "wer", "cer",
    ))


def _row_targets(row: dict[str, Any], targets: list[str], missed_terms: list[str]) -> list[str]:
    if missed_terms:
        return list(missed_terms)
    return [] if _has_reference_context(row) else list(targets)


def _add_position_matches(
    *,
    words: list[str],
    reference_words: list[str],
    missed_terms: list[str],
    sample: str,
    state: _SuggestionState,
) -> None:
    for term in missed_terms:
        term_words = _words(term)
        if not term_words:
            continue
        size = len(term_words)
        for i in range(0, len(reference_words) - size + 1):
            if _normalize(" ".join(reference_words[i:i + size])) != _normalize(term):
                continue
            for candidate_size in sorted({size, size + 1}):
                if i + candidate_size > len(words):
                    continue
                source = " ".join(words[i:i + candidate_size])
                _add_suggestion(
                    source,
                    term,
                    confidence=max(0.70, _similarity(source, term)),
                    reason="term_miss_position_match",
                    sample=sample,
                    state=state,
                )


def _candidate_ngram_sizes(target: str) -> list[int]:
    target_len = max(1, len(_words(target)))
    return [size for size in sorted({target_len - 1, target_len, target_len + 1}) if size > 0]


def _add_fuzzy_matches(
    *,
    words: list[str],
    text_norm: str,
    row_targets: list[str],
    missed_terms: list[str],
    min_confidence: float,
    sample: str,
    state: _SuggestionState,
) -> None:
    for target in row_targets:
        target_norm = _normalize(target)
        if not target_norm or target_norm in text_norm:
            continue
        for source, confidence in _fuzzy_candidates(words, target, target_norm, min_confidence, state):
            _add_suggestion(
                source,
                target,
                confidence=confidence,
                reason="term_miss_fuzzy_match" if target in missed_terms else "dictionary_fuzzy_match",
                sample=sample,
                state=state,
            )


def _fuzzy_candidates(
    words: list[str],
    target: str,
    target_norm: str,
    min_confidence: float,
    state: _SuggestionState,
) -> Iterable[tuple[str, float]]:
    for size in _candidate_ngram_sizes(target):
        for source in _ngrams(words, size) or []:
            confidence = _candidate_confidence(source, target, target_norm, min_confidence, state)
            if confidence is not None:
                yield source, confidence


def _candidate_confidence(
    source: str,
    target: str,
    target_norm: str,
    min_confidence: float,
    state: _SuggestionState,
) -> float | None:
    source_norm = _normalize(source)
    if source_norm == target_norm or (source_norm, target_norm) in state.existing:
        return None
    confidence = _similarity(source, target)
    return confidence if confidence >= min_confidence else None


def _suggestions_from_state(state: _SuggestionState) -> list[ReplacementSuggestion]:
    grouped = [
        ReplacementSuggestion(
            source=source,
            target=target,
            count=count,
            confidence=state.best[(source, target)],
            reason=state.reasons[(source, target)],
            samples=state.samples[(source, target)][:5],
        )
        for (source, target), count in state.counts.items()
    ]
    return sorted(grouped, key=lambda s: (-s.count, -s.confidence, s.target.casefold()))


def suggest_replacements_from_rows(
    rows: list[dict[str, Any]],
    *,
    min_confidence: float = 0.62,
) -> list[ReplacementSuggestion]:
    targets = _known_targets(rows)
    state = _SuggestionState(existing=_existing_replacement_pairs())

    for row in rows:
        text = str(row.get("text") or row.get("dictionary_text") or row.get("raw_text") or "")
        if not text:
            continue
        sample = str(row.get("corpus_id") or row.get("target_title") or row.get("source_file") or "")
        missed_terms = _missed_terms(row)
        row_targets = _row_targets(row, targets, missed_terms)
        words = _words(text)
        reference_words = _words(str(row.get("reference_text") or ""))
        _add_position_matches(
            words=words,
            reference_words=reference_words,
            missed_terms=missed_terms,
            sample=sample,
            state=state,
        )
        _add_fuzzy_matches(
            words=words,
            text_norm=_normalize(text),
            row_targets=row_targets,
            missed_terms=missed_terms,
            min_confidence=min_confidence,
            sample=sample,
            state=state,
        )
    return _suggestions_from_state(state)


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
