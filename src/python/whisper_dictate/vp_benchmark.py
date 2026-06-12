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
from typing import Any, Iterable

from whisper_dictate.vp_config import get_value
from whisper_dictate.vp_cli import _resolve_device

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
        if backend not in ("whisper", "parakeet", "openai"):
            raise ValueError(
                f"unsupported benchmark backend {backend!r}; expected whisper, parakeet or openai")
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
        if spec.backend == "parakeet":
            env["VOICEPI_PARAKEET_MODEL"] = spec.model
        else:
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
    if spec.backend == "parakeet":
        from whisper_dictate.vp_parakeet import ParakeetModel, resolve_parakeet_model_name
        model_name = resolve_parakeet_model_name(spec.model)
        return (
            ParakeetModel(model_name, device=device, compute_type=compute_type),
            model_name,
            device,
            compute_type,
        )
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
) -> list[tuple[str | Path, Any | None]]:
    if corpus_items:
        return [(item.audio, item) for item in corpus_items]
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
        event = skipped_event(item, "audio file missing")
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
) -> list[dict[str, Any]]:
    specs = parse_backend_specs(backend_specs)
    results: list[dict[str, Any]] = []
    corpus_items = load_corpus(corpus_manifest) if corpus_manifest else []
    work = _benchmark_work(audio_files, corpus_items)
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
        "scored": len(scored),
        "avg_wer": avg_wer,
        "avg_cer": avg_cer,
    }


def format_summary_line(summary: dict[str, Any]) -> str:
    """Render a one-line, human-readable summary prefixed with ``[benchmark]``.

    The UI surfaces this exact line in the runtime log so the user sees a concise
    pass count + overall WER without parsing the per-item JSONL.
    """
    parts = [f"{summary['passed']}/{summary['total']} passed"]
    if summary["skipped"]:
        parts.append(f"{summary['skipped']} skipped")
    if summary["failed"]:
        parts.append(f"{summary['failed']} failed")
    if summary["avg_wer"] is not None:
        parts.append(f"avg WER {summary['avg_wer'] * 100:.1f}%")
    if summary["avg_cer"] is not None:
        parts.append(f"avg CER {summary['avg_cer'] * 100:.1f}%")
    return "[benchmark] " + ", ".join(parts)


def run_corpus_benchmark(
    corpus_manifest: str | Path | None = None,
    backend_specs: str | Iterable[str] | None = None,
    *,
    output_jsonl: str | Path | None = None,
) -> dict[str, Any]:
    """Run the golden corpus and print a concise summary line, then return it.

    This is the single UI-invokable entry the "Run benchmark" button drives via
    ``--run-benchmark``. It defaults to ``benchmark/corpus.json`` so the button
    needs no arguments, runs every corpus item through the configured backend
    (emitting the usual per-item JSONL to stdout), then prints one
    ``[benchmark] …`` summary line so the result lands in the runtime log.
    """
    manifest = corpus_manifest or DEFAULT_CORPUS_MANIFEST
    results = run_benchmark(
        None,
        backend_specs,
        output_jsonl=output_jsonl,
        corpus_manifest=manifest,
    )
    summary = summarize_results(results)
    print(format_summary_line(summary), flush=True)
    return summary
