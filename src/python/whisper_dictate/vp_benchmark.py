"""Benchmark/evaluation harness for STT backends."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Iterable

if TYPE_CHECKING:
    from whisper_dictate.vp_corpus_profile import CorpusProfile

from whisper_dictate.vp_config import appdata_dir, get_value
from whisper_dictate.vp_cli import _resolve_device
from whisper_dictate.vp_benchmark_paths import (
    appdata_audio_dir,
    corpus_search_paths,
    resolve_corpus_manifest,
    resolve_item_audio,
)
from whisper_dictate.vp_benchmark_report import (
    MISSING_AUDIO_REASON,
    format_summary_line,
    summarize_results,
)

# Re-exported here so existing callers/tests keep importing these helpers from
# ``vp_benchmark``; the pure implementations live in ``vp_benchmark_paths`` /
# ``vp_benchmark_report`` to keep this module under the size limit.
__all__ = [
    "MISSING_AUDIO_REASON",
    "appdata_audio_dir",
    "corpus_search_paths",
    "format_summary_line",
    "resolve_corpus_manifest",
    "resolve_item_audio",
    "summarize_results",
]

# Default golden-corpus manifest, relative to the app root (the worker's working
# directory). The "Run benchmark" UI button drives `--run-benchmark`, which runs
# this manifest so the button needs no arguments.
DEFAULT_CORPUS_MANIFEST = "benchmark/corpus.json"


@dataclass(frozen=True)
class CorpusItem:
    id: str
    text: str
    audio: Path
    language: str = ""
    category: str = ""
    terms: tuple[str, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class BackendSpec:
    raw: str
    backend: str
    model: str | None = None


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


def parse_backend_specs(spec: str | Iterable[str] | None = None) -> list[BackendSpec]:
    if spec is None:
        spec = get_value("VOICEPI_STT_BACKEND", "whisper") or "whisper"
    if isinstance(spec, str):
        parts = [p.strip() for p in spec.split(",")]
    else:
        parts = [str(p).strip() for p in spec]
    out: list[BackendSpec] = []
    for part in parts:
        if not part:
            continue
        backend, sep, model = part.partition(":")
        backend = backend.strip().lower()
        model = model.strip() if sep else None
        # Wave 8 of #348: a saved `stt_backend = "parakeet"` is migrated to
        # whisper persistently at config-load time on the Rust side, but the
        # System tab's "Run benchmark" path can reach here BEFORE that save
        # round-trip happens — `parse_backend_specs(None)` reads the legacy
        # value back through Python's `get_value`. Normalise it the same way
        # the supervisor would so an upgraded user benchmarks Whisper instead
        # of hitting `unsupported benchmark backend 'parakeet'` (Codex P2 on
        # PR #410). Keeps the spec list informative: we replace the backend
        # token but preserve the raw input so the worker log still shows what
        # the user supplied.
        if backend == "parakeet":
            backend = "whisper"
            part = part.replace("parakeet", "whisper", 1) if "parakeet" in part else "whisper"
        if backend not in ("whisper", "openai"):
            raise ValueError(
                f"unsupported benchmark backend {backend!r}; expected whisper or openai")
        out.append(BackendSpec(raw=part, backend=backend, model=model or None))
    if not out:
        raise ValueError("at least one benchmark backend is required")
    return out


def _event_from_stdout(stdout: str) -> dict[str, Any] | None:
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            return obj
    return None


def run_one(
    audio_file: str | Path,
    spec: BackendSpec,
    *,
    python_exe: str = sys.executable,
    app_path: str | Path | None = None,
    base_env: dict[str, str] | None = None,
    timeout_s: int = 900,
) -> dict[str, Any]:
    env = dict(os.environ if base_env is None else base_env)
    src_root = Path(__file__).resolve().parents[1]
    env["PYTHONPATH"] = (
        str(src_root)
        if not env.get("PYTHONPATH")
        else str(src_root) + os.pathsep + env["PYTHONPATH"]
    )
    env["VOICEPI_STT_BACKEND"] = spec.backend
    if spec.model:
        # Wave 8 of #348: only whisper + openai remain, so the spec model
        # always maps to VOICEPI_MODEL (no per-backend env-var routing).
        env["VOICEPI_MODEL"] = spec.model
    if app_path:
        cmd = [python_exe, str(app_path)]
    else:
        cmd = [python_exe, "-m", "whisper_dictate.runtime"]
    cmd.extend(["--transcribe-file", str(audio_file), "--json"])
    t0 = time.monotonic()
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        timeout=timeout_s,
    )
    elapsed = time.monotonic() - t0
    event = _event_from_stdout(proc.stdout)
    if event is None:
        event = {
            "event": "benchmark_result",
            "text": "",
            "raw_text": "",
            "source_file": str(audio_file),
        }
    event.update({
        "event": "benchmark_result",
        "benchmark_backend_spec": spec.raw,
        "benchmark_backend": spec.backend,
        "benchmark_model": spec.model,
        "benchmark_elapsed_s": elapsed,
        "benchmark_success": proc.returncode == 0 and bool(event.get("text")),
        "benchmark_returncode": proc.returncode,
    })
    if proc.returncode != 0:
        event["benchmark_error"] = (proc.stderr or proc.stdout).strip()[-4000:]
    return event


def _load_model_for_spec(spec: BackendSpec) -> tuple[Any, str, str, str]:
    device, compute_type = _resolve_device(get_value("VOICEPI_DEVICE", "auto") or "auto")
    # Wave 8 of #348 removed the Parakeet branch here together with the
    # backend; only the cloud and local Whisper paths remain.
    if spec.backend == "openai":
        from whisper_dictate.vp_external_api import ExternalTranscriptionModel
        model_name = spec.model or get_value("VOICEPI_STT_MODEL", "gpt-4o-mini-transcribe")
        return (
            ExternalTranscriptionModel(model_name),
            model_name,
            "api",
            "remote",
        )

    from faster_whisper import WhisperModel
    model_name = spec.model or get_value("VOICEPI_MODEL", "large-v3-turbo")
    return (
        WhisperModel(model_name, device=device, compute_type=compute_type),
        model_name,
        device,
        compute_type,
    )


def _run_loaded_model(audio_file: str | Path, item: Any | None, spec: BackendSpec,
                      loaded: tuple[Any, str, str, str]) -> dict[str, Any]:
    from whisper_dictate.runtime import transcribe_file_event

    model, model_name, device, compute_type = loaded
    lang = getattr(item, "language", None) or get_value("VOICEPI_LANG")
    t0 = time.monotonic()
    try:
        event = transcribe_file_event(
            model,
            audio_file,
            lang,
            model_name=model_name,
            stt_backend=spec.backend,
            device=device,
            compute_type=compute_type,
        )
        success = bool(event.get("text"))
        returncode = 0
    except Exception as exc:  # noqa: BLE001 - benchmarks should record failures
        event = {
            "event": "benchmark_result",
            "text": "",
            "raw_text": "",
            "source_file": str(audio_file),
            "benchmark_error": str(exc),
        }
        success = False
        returncode = 1

    event.update({
        "event": "benchmark_result",
        "benchmark_elapsed_s": time.monotonic() - t0,
        "benchmark_success": success,
        "benchmark_returncode": returncode,
    })
    return event


def _benchmark_work(
    audio_files: Iterable[str | Path] | None,
    corpus_items: list[Any],
    appdata: str | Path | None = None,
) -> list[tuple[str | Path, Any | None]]:
    if corpus_items:
        # Resolve each item's audio here (once), so both the missing-file skip
        # check and the per-spec model-load gating below see the per-user
        # fallback path when the manifest-relative recording is absent.
        return [(resolve_item_audio(item.audio, appdata), item) for item in corpus_items]
    return [(path, None) for path in (audio_files or [])]


def _open_jsonl_sink(output_jsonl: str | Path | None):
    if not output_jsonl:
        return None
    out_path = Path(output_jsonl)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    return out_path.open("a", encoding="utf-8")


def _run_benchmark_item(
    audio_file: str | Path,
    item: Any | None,
    spec: BackendSpec,
    loaded: tuple[Any, str, str, str] | None,
) -> dict[str, Any]:
    if item is not None and not Path(audio_file).exists():
        event = skipped_event(item, MISSING_AUDIO_REASON)
    elif item is not None:
        event = _run_loaded_model(audio_file, item, spec, loaded)
        annotate_event(event, item)
    else:
        event = run_one(audio_file, spec)
    event.update({
        "benchmark_backend_spec": spec.raw,
        "benchmark_backend": spec.backend,
        "benchmark_model": spec.model,
    })
    return event


def _write_benchmark_event(sink, event: dict[str, Any]) -> None:
    line = json.dumps(event, ensure_ascii=False, separators=(",", ":"))
    if sink:
        sink.write(line + "\n")
        sink.flush()
        return
    print(line, flush=True)


def run_benchmark(
    audio_files: Iterable[str | Path] | None,
    backend_specs: str | Iterable[str] | None = None,
    *,
    output_jsonl: str | Path | None = None,
    corpus_manifest: str | Path | None = None,
    appdata: str | Path | None = None,
    profile: "CorpusProfile | None" = None,
) -> list[dict[str, Any]]:
    specs = parse_backend_specs(backend_specs)
    results: list[dict[str, Any]] = []
    corpus_items = load_corpus(corpus_manifest) if corpus_manifest else []
    if profile is not None and corpus_items:
        from whisper_dictate.vp_corpus_profile import filter_corpus_items
        corpus_items = filter_corpus_items(corpus_items, profile)
        if not corpus_items:
            raise ValueError(
                f"corpus profile ({profile.describe()}) matched no items")
    work = _benchmark_work(audio_files, corpus_items, appdata)
    if not work:
        raise ValueError("at least one benchmark file or corpus item is required")
    sink = _open_jsonl_sink(output_jsonl)
    try:
        for spec in specs:
            loaded = None
            if corpus_items and any(Path(audio_file).exists() for audio_file, _ in work):
                loaded = _load_model_for_spec(spec)
            for audio_file, item in work:
                event = _run_benchmark_item(audio_file, item, spec, loaded)
                results.append(event)
                _write_benchmark_event(sink, event)
    finally:
        if sink:
            sink.close()
    return results


def run_corpus_benchmark(
    corpus_manifest: str | Path | None = None,
    backend_specs: str | Iterable[str] | None = None,
    *,
    output_jsonl: str | Path | None = None,
    app_root: str | Path | None = None,
    profile: "CorpusProfile | None" = None,
) -> dict[str, Any] | None:
    """Run the golden corpus and print a concise summary line, then return it.

    This is the single UI-invokable entry the "Run benchmark" button drives via
    ``--run-benchmark``. It needs no arguments: the corpus manifest is resolved
    via :func:`resolve_corpus_manifest` (explicit arg → ``<app_root>/benchmark``
    → per-user appdata), so it works in a dev checkout AND the installed app
    (which now ships the manifest). Every corpus item is run through the
    configured backend (emitting the usual per-item JSONL to stdout), then one
    ``[benchmark] …`` summary line is printed so the result lands in the runtime
    log.

    When NO corpus is found anywhere, it prints one clear ``[benchmark] no corpus
    manifest found …`` line and returns ``None`` (the caller exits 0) — a clean,
    visible outcome rather than a crash, so the button never feels dead.
    """
    appdata = appdata_dir()
    # Default the app root to the worker's CWD ("."): the Rust controller runs
    # the worker with cwd == app-root and passes --app-root, but a bare dev/CLI
    # invocation from the repo root has no --app-root, and "." then resolves the
    # committed dev-checkout `benchmark/corpus.json`. Same code path either way.
    root = app_root if app_root is not None else "."
    manifest = resolve_corpus_manifest(root, corpus_manifest, appdata)
    if manifest is None:
        looked = ", ".join(str(p) for p in corpus_search_paths(root, appdata))
        print(
            f"[benchmark] no corpus manifest found (looked: {looked}) — "
            "see docs/CONFIGURATION.md (Benchmark corpus)",
            flush=True,
        )
        return None
    results = run_benchmark(
        None,
        backend_specs,
        output_jsonl=output_jsonl,
        corpus_manifest=manifest,
        appdata=appdata,
        profile=profile,
    )
    summary = summarize_results(results)
    audio_hint = appdata_audio_dir(appdata)
    print(format_summary_line(summary, audio_hint_path=audio_hint), flush=True)
    return summary
