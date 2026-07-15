"""Pure path-resolution helpers for the golden-benchmark corpus.

Split out of ``vp_benchmark`` so the resolution logic stays small, side-effect
free (only ``Path.exists`` checks) and independently unit-testable. The "Run
benchmark" button needs the corpus to resolve in the installed app as well as a
dev checkout, and recordings (gitignored, user-local) to survive reinstalls —
both are decided here.
"""
from __future__ import annotations

from pathlib import Path

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
