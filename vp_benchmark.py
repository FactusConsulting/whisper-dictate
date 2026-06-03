"""Benchmark/evaluation harness for STT backends."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from vp_config import get_value
from vp_corpus import annotate_event, load_corpus, skipped_event
from vp_device import _resolve_device


@dataclass(frozen=True)
class BackendSpec:
    raw: str
    backend: str
    model: str | None = None


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
    app = Path(app_path) if app_path else Path(__file__).with_name("voice_pi.py")
    env = dict(os.environ if base_env is None else base_env)
    env["VOICEPI_STT_BACKEND"] = spec.backend
    if spec.model:
        if spec.backend == "parakeet":
            env["VOICEPI_PARAKEET_MODEL"] = spec.model
        else:
            env["VOICEPI_MODEL"] = spec.model
    cmd = [
        python_exe, str(app),
        "--transcribe-file", str(audio_file),
        "--json",
    ]
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
        from vp_parakeet import ParakeetModel, resolve_parakeet_model_name
        model_name = resolve_parakeet_model_name(spec.model)
        return (
            ParakeetModel(model_name, device=device, compute_type=compute_type),
            model_name,
            device,
            compute_type,
        )
    if spec.backend == "openai":
        from vp_external_api import ExternalTranscriptionModel
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
    from vp_file_transcribe import transcribe_file_event

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
