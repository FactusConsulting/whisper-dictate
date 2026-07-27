"""Corpus manifest resolution + loading helpers.

Split out of the now-retired ``vp_benchmark`` module so the resolution logic
stays small, side-effect free (only ``Path.exists`` checks) and independently
unit-testable. Retained after step 2 of the ``vp_benchmark`` retirement (#348)
because ``vp_corpus_record.py`` (still Python pending its own step 2) needs
``load_corpus`` / ``resolve_corpus_manifest`` / ``appdata_audio_dir`` to
resolve the corpus item to record. Native benchmarking now lives in
``src/rust/benchmark/native.rs``; the corresponding Rust corpus helpers are
``src/rust/corpus.rs`` + ``src/rust/benchmark/paths.rs``.
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Relative manifest path under both the app root (dev checkout + shipped) and the
# per-user appdata dir. Kept as a tuple so `resolve_corpus_manifest` builds the
# same `benchmark/corpus.json` under either base.
_CORPUS_REL_PARTS = ("benchmark", "corpus.json")
# Per-user audio directory: %APPDATA%/WhisperDictate/benchmark/audio (Windows) or
# the XDG equivalent. Corpus recordings are user-local and gitignored, so the
# per-item audio fallback looks here when a recording is missing next to the
# shipped manifest — letting users keep recordings in a dir that survives
# reinstalls.
_AUDIO_REL_PARTS = ("benchmark", "audio")


@dataclass(frozen=True)
class CorpusItem:
    """One fully-parsed corpus item.

    Duck-typed by :mod:`vp_corpus_profile`; the Rust mirror lives in
    ``src/rust/corpus.rs::CorpusItem``.
    """
    id: str
    text: str
    audio: Path
    language: str = ""
    category: str = ""
    terms: tuple[str, ...] = field(default_factory=tuple)


def resolve_corpus_manifest(
    app_root: str | Path | None,
    explicit: str | Path | None,
    appdata: str | Path | None,
) -> Path | None:
    """Resolve the golden-corpus manifest in priority order, returns None if absent.

    Pure (no model load / process side effects) so it is unit-testable. Priority:

    1. ``explicit`` — an ``--benchmark-corpus PATH`` arg, used verbatim if given
       (returned even when it does not exist, so the caller reports that path).
    2. ``<app_root>/benchmark/corpus.json`` — the dev checkout layout and the
       file the installer now ships, so the button works out of the box.
    3. ``<appdata>/benchmark/corpus.json`` — a user-managed manifest that
       survives reinstalls (same per-user dir as config.json/dictionary.json).

    Returns the first existing candidate (or the explicit path), else ``None``.
    """
    if explicit:
        return Path(explicit)
    for candidate in corpus_search_paths(app_root, appdata):
        if candidate.exists():
            return candidate
    return None


def corpus_search_paths(
    app_root: str | Path | None,
    appdata: str | Path | None,
) -> list[Path]:
    """The manifest locations `resolve_corpus_manifest` checks, for error display.

    Mirrors the app-root + appdata candidates (sans the explicit arg, which only
    exists when the user passes one) so the "no corpus found" message can list
    exactly where the worker looked.
    """
    paths: list[Path] = []
    if app_root:
        paths.append(Path(app_root).joinpath(*_CORPUS_REL_PARTS))
    if appdata:
        paths.append(Path(appdata).joinpath(*_CORPUS_REL_PARTS))
    return paths


def appdata_audio_dir(appdata: str | Path) -> Path:
    """The per-user benchmark audio dir under the given appdata base."""
    return Path(appdata).joinpath(*_AUDIO_REL_PARTS)


def resolve_item_audio(audio: str | Path, appdata: str | Path | None) -> Path:
    """Resolve a corpus item's audio file, falling back to the per-user dir.

    Pure (only filesystem ``exists`` checks). Returns ``audio`` unchanged when it
    exists (the manifest-relative recording shipped/recorded in place). When it is
    missing AND a per-user appdata dir is given, returns
    ``<appdata>/benchmark/audio/<filename>`` if that exists — so users can keep
    recordings in a stable dir that survives reinstalls. Otherwise returns the
    original ``audio`` (the caller then records it as a skip).
    """
    audio_path = Path(audio)
    if audio_path.exists() or not appdata:
        return audio_path
    fallback = appdata_audio_dir(appdata) / audio_path.name
    return fallback if fallback.exists() else audio_path


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
    """Load and parse a golden-corpus manifest.

    Retained for :mod:`vp_corpus_record` which still uses the Python worker to
    record reference audio; the native Rust benchmark runner
    (``src/rust/benchmark/native.rs``) uses ``src/rust/corpus.rs::load_corpus``
    for its own path.
    """
    data, base = _load_manifest(path)
    audio_dir = Path(str(data.get("audio_dir", "")))
    items = data.get("items")
    if not isinstance(items, list):
        raise ValueError("corpus manifest must contain an items array")
    seen: set[str] = set()
    return [_parse_item(raw, audio_dir=audio_dir, base=base, seen=seen) for raw in items]
