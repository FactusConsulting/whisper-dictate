"""CLI orchestration for corpus->dictionary training (Feature A + profile B).

Glues the PURE logic (:mod:`vp_dictionary_training`, :mod:`vp_corpus_profile`) to
the IO (:mod:`vp_dictionary_store`, :mod:`vp_benchmark` corpus loading). This is
the only place that reads/writes files; the math/filters stay pure and unit
tested. Two entry points map to the two CLI commands:

* :func:`run_build_from_corpus` (``--dictionary-build-from-corpus``) — extract
  candidate terms from the (optionally profile-filtered) corpus reference TEXT and
  APPEND+DEDUP them into the dictionary. Defaults to a PREVIEW; only writes when
  ``apply=True`` (``--apply``). It NEVER records or reads corpus audio.
* :func:`run_suggest_from_misses` (``--dictionary-suggest-terms``) — read an
  annotated benchmark JSONL and surface the domain terms the model missed as
  SUGGESTED dictionary additions (preview), writing only on ``apply=True``.

Both print a clear preview the user/automation confirms before any change, honour
``--language`` / ``--category`` profile selection, and emit ``--json`` for
tooling. Kept egui-agnostic: no UI hooks (deferred to the egui 0.34 branch).
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from whisper_dictate.vp_corpus_profile import build_profile, filter_corpus_items
from whisper_dictate.vp_dictionary_store import (
    load_dictionary_document,
    resolve_dictionary_path,
    terms_from_document,
    write_terms,
)
from whisper_dictate.vp_dictionary_training import (
    extract_candidate_terms,
    merge_terms,
    suggest_terms_from_misses,
)


def _load_corpus_items(corpus_manifest: str | Path | None, app_root: str | Path | None):
    """Resolve + load the golden corpus the same way the benchmark does.

    Returns the parsed ``CorpusItem`` list, or raises ``LookupError`` with the
    searched locations when no manifest exists — a clear, actionable message.
    """
    from whisper_dictate.vp_benchmark import load_corpus
    from whisper_dictate.vp_benchmark_paths import corpus_search_paths, resolve_corpus_manifest
    from whisper_dictate.vp_config import appdata_dir

    appdata = appdata_dir()
    root = app_root if app_root is not None else "."
    manifest = resolve_corpus_manifest(root, corpus_manifest, appdata)
    if manifest is None or not Path(manifest).exists():
        looked = ", ".join(str(p) for p in corpus_search_paths(root, appdata))
        raise LookupError(f"no benchmark corpus found (looked: {looked})")
    return load_corpus(manifest)


def _read_jsonl(path: str | Path) -> list[dict[str, Any]]:
    """Read newline-delimited JSON objects, skipping blanks/unparseable lines."""
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


def _emit_json(payload: dict[str, Any]) -> None:
    print(json.dumps(payload, ensure_ascii=False, sort_keys=True), flush=True)


def run_build_from_corpus(
    *,
    corpus_manifest: str | Path | None = None,
    app_root: str | Path | None = None,
    dictionary_path: str | Path | None = None,
    language: str | None = None,
    category: str | None = None,
    min_count: int = 1,
    apply: bool = False,
    as_json: bool = False,
) -> int:
    """``--dictionary-build-from-corpus``: grow the dictionary from corpus TEXT.

    Loads the corpus, applies the language/category profile, extracts candidate
    domain terms from the reference TEXT, merges them (append + case-insensitive
    dedup) against the existing dictionary, and PREVIEWS what would be added. Only
    writes the dictionary when ``apply`` is True. Returns a process exit code (0 on
    success, 1 on a corpus/dictionary error). Never records or reads audio.
    """
    profile = build_profile(language=language, category=category)
    try:
        items = filter_corpus_items(_load_corpus_items(corpus_manifest, app_root), profile)
    except LookupError as exc:
        return _fail(str(exc), as_json)

    dict_path = resolve_dictionary_path(dictionary_path)
    try:
        document = load_dictionary_document(dict_path)
    except ValueError as exc:
        return _fail(str(exc), as_json)
    existing = terms_from_document(document)

    candidates = extract_candidate_terms(
        [item.text for item in items],
        item_terms=[item.terms for item in items],
        item_ids=[item.id for item in items],
        min_count=min_count,
    )
    preview = merge_terms(existing, candidates)

    wrote = False
    if apply and preview.added:
        write_terms(dict_path, preview.result_terms, base=document)
        wrote = True

    _report_build(
        preview=preview,
        candidates=candidates,
        profile=profile,
        items=items,
        dict_path=dict_path,
        wrote=wrote,
        apply=apply,
        as_json=as_json,
    )
    return 0


def run_suggest_from_misses(
    jsonl_path: str | Path,
    *,
    dictionary_path: str | Path | None = None,
    min_count: int = 1,
    apply: bool = False,
    as_json: bool = False,
) -> int:
    """``--dictionary-suggest-terms``: suggest dictionary terms from benchmark misses.

    Reads an annotated benchmark JSONL, surfaces the domain terms the model missed
    (``term_misses``) as SUGGESTED additions flagged against the current
    dictionary, and PREVIEWS them. Writes the not-yet-present suggestions only when
    ``apply`` is True. Returns a process exit code. Reads result TEXT only — no
    audio.
    """
    dict_path = resolve_dictionary_path(dictionary_path)
    try:
        document = load_dictionary_document(dict_path)
    except ValueError as exc:
        return _fail(str(exc), as_json)
    existing = terms_from_document(document)

    try:
        rows = _read_jsonl(jsonl_path)
    except OSError as exc:
        return _fail(str(exc), as_json)

    suggestions = suggest_terms_from_misses(rows, existing_terms=existing, min_count=min_count)
    new_terms = [s.term for s in suggestions if not s.already_in_dictionary]

    wrote = False
    if apply and new_terms:
        preview = merge_terms(existing, new_terms)
        write_terms(dict_path, preview.result_terms, base=document)
        wrote = True

    _report_suggestions(
        suggestions=suggestions,
        new_terms=new_terms,
        dict_path=dict_path,
        wrote=wrote,
        apply=apply,
        as_json=as_json,
    )
    return 0


def _fail(message: str, as_json: bool) -> int:
    if as_json:
        _emit_json({"error": message})
    else:
        print(f"error: {message}", flush=True)
    return 1


def _report_build(*, preview, candidates, profile, items, dict_path, wrote, apply, as_json) -> None:
    if as_json:
        _emit_json({
            "command": "build-from-corpus",
            "dictionary": str(dict_path),
            "profile": profile.describe(),
            "corpus_items": len(items),
            "existing_terms": preview.existing_count,
            "added": preview.added,
            "skipped_existing": preview.skipped_existing,
            "candidates": [
                {"term": c.term, "count": c.count, "reason": c.reason}
                for c in candidates
            ],
            "applied": wrote,
        })
        return
    print(
        f"[dictionary] build-from-corpus reads corpus reference TEXT only "
        f"(never records audio).",
        flush=True,
    )
    print(f"  corpus selection: {profile.describe()}  ({len(items)} item(s))", flush=True)
    print(f"  dictionary: {dict_path}  ({preview.existing_count} existing term(s))", flush=True)
    if preview.added:
        print(f"  would add {preview.added_count} new term(s):", flush=True)
        for term in preview.added:
            print(f"    + {term}", flush=True)
    else:
        print("  no new terms to add (all candidates already present).", flush=True)
    if preview.skipped_existing:
        print(f"  skipped {len(preview.skipped_existing)} already present.", flush=True)
    if wrote:
        print(f"  WROTE {preview.added_count} term(s) to {dict_path}", flush=True)
    elif preview.added:
        print("  PREVIEW only — re-run with --apply to write these terms.", flush=True)


def _report_suggestions(*, suggestions, new_terms, dict_path, wrote, apply, as_json) -> None:
    if as_json:
        _emit_json({
            "command": "suggest-from-benchmark-misses",
            "dictionary": str(dict_path),
            "suggestions": [
                {
                    "term": s.term,
                    "count": s.count,
                    "samples": list(s.samples),
                    "already_in_dictionary": s.already_in_dictionary,
                }
                for s in suggestions
            ],
            "new_terms": new_terms,
            "applied": wrote,
        })
        return
    print(
        "[dictionary] suggest-from-benchmark-misses reads benchmark result TEXT "
        "only (never records audio).",
        flush=True,
    )
    print(f"  dictionary: {dict_path}", flush=True)
    if not suggestions:
        print("  no missed domain terms found in the benchmark results.", flush=True)
        return
    print(f"  {len(suggestions)} suggested term(s) from benchmark misses:", flush=True)
    for s in suggestions:
        mark = "(already in dictionary)" if s.already_in_dictionary else "NEW"
        samples = f"  samples={', '.join(s.samples[:3])}" if s.samples else ""
        print(f"    {mark:>22}  {s.term!r}  count={s.count}{samples}", flush=True)
    if wrote:
        print(f"  WROTE {len(new_terms)} new term(s) to {dict_path}", flush=True)
    elif new_terms:
        print(
            f"  PREVIEW only — re-run with --apply to add the {len(new_terms)} "
            "NEW term(s).",
            flush=True,
        )
