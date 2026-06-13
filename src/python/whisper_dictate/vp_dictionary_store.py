"""Thin IO layer for the managed ``dictionary.json`` (read terms, append terms).

Feature (A) of the 1.13 wave writes new domain terms into the SAME dictionary
file the Rust runtime loads as the Whisper prompt vocabulary. The on-disk format
is the plain JSON object the Rust ``dictionary`` command reads/writes
(``src/rust/dictionary.rs``):

    {"terms": ["Claude Code", ...], "replacements": {"Cloud Code": "Claude Code"}}

This module owns ONLY the side-effecting bits — path resolution and load/save —
so the term-extraction/merge logic in :mod:`vp_dictionary_training` stays pure and
unit-testable. On write we PRESERVE every other top-level key (``replacements``
and any future fields), exactly like the Rust writer does, so growing the term
list never clobbers a user's replacements.

Path resolution mirrors the Rust ``RuntimeDictionarySettings`` env path and the
:func:`vp_cli._default_dictionary_path` default so the file the trainer appends to
is the file dictation actually loads:

* explicit ``path`` arg (``--dictionary PATH``) wins;
* else ``VOICEPI_DICTIONARY`` (first entry if it is an ``os.pathsep`` list);
* else the per-user default (``%APPDATA%/WhisperDictate/dictionary.json`` on
  Windows, ``$XDG_CONFIG_HOME/whisper-dictate/dictionary.json`` elsewhere).

NOTE: the Rust runtime can ALSO take the dictionary path from ``config.json``
(``configured.dictionary``). Reading that requires the Rust config loader, so when
neither ``--dictionary`` nor ``VOICEPI_DICTIONARY`` is set the CLI falls back to
the documented per-user default — the same location the Rust default uses. Pass
``--dictionary`` explicitly to target a config-relocated file.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any


def default_dictionary_path() -> Path:
    """Per-user default dictionary path (mirrors ``vp_cli._default_dictionary_path``).

    Windows: ``%APPDATA%/WhisperDictate/dictionary.json``; elsewhere
    ``$XDG_CONFIG_HOME/whisper-dictate/dictionary.json`` (``~/.config`` default).
    Kept independent of ``vp_cli`` (which loads the heavy argparse surface) so this
    IO helper is cheap to import.
    """
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return Path(base) / "WhisperDictate" / "dictionary.json"
    base = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(base) / "whisper-dictate" / "dictionary.json"


def resolve_dictionary_path(path: str | Path | None = None) -> Path:
    """Resolve the dictionary file: explicit arg -> ``VOICEPI_DICTIONARY`` -> default.

    ``VOICEPI_DICTIONARY`` may be an ``os.pathsep``-separated list (the Rust side
    can load several); for *writing* we target the first non-empty entry (the
    primary, user-managed file). Returns a ``Path`` that may not exist yet.
    """
    if path:
        return Path(path)
    env = os.environ.get("VOICEPI_DICTIONARY")
    if env:
        for part in env.split(os.pathsep):
            part = part.strip()
            if part:
                return Path(part)
    return default_dictionary_path()


def load_dictionary_document(path: str | Path) -> dict[str, Any]:
    """Load the dictionary JSON object, or an empty doc when absent/blank.

    Returns the parsed top-level object so the caller can preserve unknown keys on
    write. A missing file yields ``{}`` (a fresh dictionary). Raises ``ValueError``
    for malformed JSON or a non-object root, so the CLI can report a clear error
    instead of silently dropping a user's existing terms.
    """
    p = Path(path).expanduser()
    if not p.exists():
        return {}
    raw = p.read_text(encoding="utf-8").strip()
    if not raw:
        return {}
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValueError(f"dictionary at {p} is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise ValueError(f"dictionary at {p} root must be a JSON object")
    return data


def load_terms(path: str | Path) -> list[str]:
    """Existing term strings from the dictionary (string or ``{"term": ...}`` form).

    Mirrors the Rust ``parse_dictionary_terms`` which accepts both a bare string
    and a ``{"term": "..."}`` object in the ``terms`` array. Blank/None entries are
    dropped. Used as the dedup baseline when appending corpus-derived terms.
    """
    document = load_dictionary_document(path)
    return terms_from_document(document)


def terms_from_document(document: dict[str, Any]) -> list[str]:
    """Extract the term strings from an already-loaded dictionary document (pure)."""
    out: list[str] = []
    for item in document.get("terms") or []:
        if isinstance(item, str):
            term = item.strip()
        elif isinstance(item, dict):
            term = str(item.get("term") or "").strip()
        else:
            term = ""
        if term:
            out.append(term)
    return out


def write_terms(path: str | Path, terms: list[str], *, base: dict[str, Any] | None = None) -> Path:
    """Write ``terms`` into the dictionary, preserving every other top-level key.

    ``base`` is the previously-loaded document whose non-``terms`` keys (e.g.
    ``replacements``) are kept verbatim — pass the result of
    :func:`load_dictionary_document` so a term append never drops replacements. The
    ``terms`` array is replaced with ``terms`` as plain strings (the caller has
    already merged/deduped). Parent dirs are created; output is pretty-printed with
    a trailing newline to match the Rust writer's on-disk shape. Returns the path.
    """
    p = Path(path).expanduser()
    document: dict[str, Any] = dict(base) if base else {}
    document["terms"] = list(terms)
    document.setdefault("replacements", {})
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(
        json.dumps(document, ensure_ascii=False, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    return p
