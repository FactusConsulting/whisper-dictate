"""Benchmark corpus manifest loading and scoring helpers."""
from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable


@dataclass(frozen=True)
class CorpusItem:
    id: str
    text: str
    audio: Path
    language: str = ""
    category: str = ""
    terms: tuple[str, ...] = field(default_factory=tuple)


def _load_manifest(path: str | Path) -> tuple[dict[str, Any], Path]:
    manifest = Path(path)
    data = json.loads(manifest.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError("corpus manifest root must be an object")
    return data, manifest.parent


def _item_audio(raw: dict[str, Any], item_id: str, audio_dir: Path, base: Path) -> Path:
    audio_raw = str(raw.get("audio") or (audio_dir / f"{item_id}.wav"))
    audio = Path(audio_raw)
    return audio if audio.is_absolute() else base / audio


def _parse_terms(raw: dict[str, Any], item_id: str) -> tuple[str, ...]:
    terms = raw.get("terms") or []
    if not isinstance(terms, list):
        raise ValueError(f"corpus item {item_id}: terms must be an array")
    return tuple(str(t).strip() for t in terms if str(t).strip())


def _parse_item(raw: Any, *, audio_dir: Path, base: Path, seen: set[str]) -> CorpusItem:
    if not isinstance(raw, dict):
        raise ValueError("corpus item must be an object")
    item_id = str(raw.get("id", "")).strip()
    text = str(raw.get("text", "")).strip()
    if not item_id or not text:
        raise ValueError("corpus item requires id and text")
    if item_id in seen:
        raise ValueError(f"duplicate corpus id: {item_id}")
    seen.add(item_id)
    return CorpusItem(
        id=item_id,
        text=text,
        audio=_item_audio(raw, item_id, audio_dir, base),
        language=str(raw.get("language", "")).strip(),
        category=str(raw.get("category", "")).strip(),
        terms=_parse_terms(raw, item_id),
    )


def load_corpus(path: str | Path) -> list[CorpusItem]:
    data, base = _load_manifest(path)
    audio_dir = Path(str(data.get("audio_dir", "")))
    items = data.get("items")
    if not isinstance(items, list):
        raise ValueError("corpus manifest must contain an items array")
    seen: set[str] = set()
    return [_parse_item(raw, audio_dir=audio_dir, base=base, seen=seen) for raw in items]


def _normalize_words(text: str) -> list[str]:
    return re.findall(r"[\wæøåÆØÅ]+", text.casefold(), flags=re.UNICODE)


def _levenshtein(a: Iterable[Any], b: Iterable[Any]) -> int:
    left = list(a)
    right = list(b)
    prev = list(range(len(right) + 1))
    for i, x in enumerate(left, 1):
        cur = [i]
        for j, y in enumerate(right, 1):
            cur.append(min(
                prev[j] + 1,
                cur[j - 1] + 1,
                prev[j - 1] + (0 if x == y else 1),
            ))
        prev = cur
    return prev[-1]


def wer(reference: str, hypothesis: str) -> float:
    ref = _normalize_words(reference)
    hyp = _normalize_words(hypothesis)
    if not ref:
        return 0.0 if not hyp else 1.0
    return _levenshtein(ref, hyp) / len(ref)


def cer(reference: str, hypothesis: str) -> float:
    ref = "".join(_normalize_words(reference))
    hyp = "".join(_normalize_words(hypothesis))
    if not ref:
        return 0.0 if not hyp else 1.0
    return _levenshtein(ref, hyp) / len(ref)


def term_report(terms: Iterable[str], hypothesis: str) -> dict[str, list[str]]:
    haystack = hypothesis.casefold()
    hits: list[str] = []
    misses: list[str] = []
    for term in terms:
        if term.casefold() in haystack:
            hits.append(term)
        else:
            misses.append(term)
    return {"hits": hits, "misses": misses}


def annotate_event(event: dict[str, Any], item: CorpusItem) -> dict[str, Any]:
    text = str(event.get("text") or "")
    terms = term_report(item.terms, text)
    event.update({
        "corpus_id": item.id,
        "corpus_category": item.category,
        "corpus_language": item.language,
        "reference_text": item.text,
        "reference_terms": list(item.terms),
        "wer": wer(item.text, text),
        "cer": cer(item.text, text),
        "exact_match": _normalize_words(item.text) == _normalize_words(text),
        "term_hits": terms["hits"],
        "term_misses": terms["misses"],
    })
    return event


def skipped_event(item: CorpusItem, reason: str) -> dict[str, Any]:
    return annotate_event({
        "event": "benchmark_result",
        "text": "",
        "raw_text": "",
        "source_file": str(item.audio),
        "benchmark_success": False,
        "benchmark_skipped": True,
        "benchmark_error": reason,
    }, item)
