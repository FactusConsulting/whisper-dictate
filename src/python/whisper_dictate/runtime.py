#!/usr/bin/env python3
"""whisper-dictate runtime — push-to-talk dictation orchestration.

Normally launched by the Rust controller (``whisper-dictate run``); all
behaviour comes from VOICEPI_* env vars + the JSON config. See
docs/CONFIGURATION.md for the canonical settings reference and
``whisper-dictate run --help`` for the flag list.

This module is the thin CLI entry point + the lazy-export wiring. The runtime
surface is split across focused ``vp_*`` package modules and re-exported here so
historical imports (``from whisper_dictate.runtime import ...``) keep working.
The heavy ML/audio/keyboard deps (numpy, sounddevice, faster_whisper) stay
behind lazy imports so ``--help`` and ``--doctor`` start instantly.
"""
from __future__ import annotations

import contextlib  # noqa: F401 — kept on the runtime surface for back-compat
import glob
import json  # noqa: F401 — kept on the runtime surface for back-compat
import os
import re  # noqa: F401 — kept on the runtime surface for back-compat
import site
import subprocess
import sys
import threading  # noqa: F401 — kept on the runtime surface for back-compat
import time
import wave  # noqa: F401 — kept on the runtime surface for back-compat
from pathlib import Path

def _configure_windows_stdio() -> None:
    if os.name != "nt":
        return
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure = getattr(stream, "reconfigure", None)
        isatty = getattr(stream, "isatty", None)
        if not callable(reconfigure):
            continue
        try:
            if callable(isatty) and isatty():
                continue
            reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass


_configure_windows_stdio()

# --- CUDA runtime DLL bootstrap (Windows) -------------------------------
# ctranslate2 (faster-whisper's backend) needs the CUDA runtime libs
# (cublas/cudnn). On Windows the nvidia-*-cu12 pip wheels drop those
# DLLs in site-packages\nvidia\*\bin, which is NOT on the default DLL
# search path. Mirror what LD_LIBRARY_PATH did in the old WSL build:
# register each nvidia\*\bin dir before faster_whisper is imported.
# Guarded + Windows-only so the file still imports cleanly elsewhere.
if os.name == "nt":
    try:
        for sp in site.getsitepackages():
            for d in sorted({os.path.dirname(p) for p in glob.glob(
                    os.path.join(sp, "nvidia", "*", "bin", "*.dll"))}):
                os.add_dll_directory(d)
                os.environ["PATH"] = d + os.pathsep + os.environ.get("PATH", "")
    except Exception as e:  # noqa: BLE001 — never block startup on this
        print(f"[warn] CUDA DLL bootstrap skipped: {e}", flush=True)

# --- Quiet huggingface_hub first-download noise -------------------------
# faster-whisper fetches the model via huggingface_hub on first run. On
# Windows without Developer Mode the cache prints a long symlinks warning,
# and recent HF versions emit an "unauthenticated requests" nag for
# anonymous downloads. Neither is actionable for a public model fetch —
# they just look like errors to new users. Suppress at multiple layers
# (env gates, Python warnings, HF logger level) to cover both emission
# paths across HF versions. Must run BEFORE any HF code imports.
os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
os.environ.setdefault("HF_HUB_VERBOSITY", "error")
import logging  # noqa: E402
import warnings  # noqa: E402
warnings.filterwarnings("ignore", module=r"huggingface_hub.*")
try:
    import huggingface_hub  # noqa: E402, F401 — registers the logger
    logging.getLogger("huggingface_hub").setLevel(logging.ERROR)
except Exception:  # noqa: BLE001 — never block startup on this
    pass

# faster_whisper and numpy are imported lazily so --help and smoke tests stay
# independent of ML/audio/keyboard backends. The CUDA DLL bootstrap above must
# still run BEFORE faster_whisper is first imported, which the lazy import
# preserves.

# --- Module surface re-exports for tests and downstream imports ---------
# The split into focused package modules keeps this module focused on runtime
# orchestration while still exposing the historical runtime surface.
from whisper_dictate.vp_cli import (  # noqa: E402
    DEVICE, INJECT_MODE, KEY, LANG, MODEL_NAME, QUIT_COUNT, QUIT_KEY,
    QUIT_WINDOW_MS,
    VALID_INJECT_MODES,
    VALID_DEVICES, _resolve_device,
    _apply_local_only_network_lock, _print_effective_config, build_arg_parser,
)
from whisper_dictate.vp_inject import InjectMixin, ydotool_socket_path, ydotoold_ready  # noqa: E402,F401
from whisper_dictate.vp_postprocess import load_postprocess_settings, postprocess_text  # noqa: E402,F401
from whisper_dictate.vp_config import (  # noqa: E402
    apply_config_to_environ, config_mtime, effective_config, get_value, load_config,  # noqa: F401
)
from whisper_dictate.vp_doctor import (  # noqa: E402,F401
    Check, run_doctor, _base_checks, _linux_checks, _print_checks,
    _print_fix_hints, _in_group, _can_import, _event_devices_readable,
    _ydotoold_process_detail,
)
from whisper_dictate.vp_audio_ducking import (  # noqa: E402,F401
    AudioDucker, register_active_ducker, restore_all_duckers,
)
from whisper_dictate.vp_rust import _rust_helper, _rust_json  # noqa: E402,F401
from whisper_dictate.vp_history import (  # noqa: E402,F401
    _append_jsonl, _append_history, append_history, default_history_path,
    history_path, history_enabled, _history_event, read_history, last_history,
    copy_last_to_clipboard, reinject_last, _run_rust_history_command,
    run_history_command,
)
from whisper_dictate.vp_events import (  # noqa: E402,F401
    _apply_profile_settings, _audio_level_metrics, _audio_meter_level_from_dbfs,
    _base_event, _compact_text, _emit_json, _emit_worker_event,
    _print_model_capacity, _run_command_hook_and_annotate,
    _select_active_channel_pcm, _sounddevice_capture_channel_candidates,
    _sounddevice_input_channels, _sounddevice_input_info, _sounddevice_input_name,
    _sounddevice_stream_kwargs, SOUNDDEVICE_START_BLOCK_MS,
)
from whisper_dictate.vp_format import (  # noqa: E402,F401
    FormatCommandResult, apply_format_commands, _format_command_set,
    _normalize_format_command_set,
)
from whisper_dictate.vp_audio_file import (  # noqa: E402,F401
    analyze_calibration_audio, calibrate_file, calibrate_microphone,
    load_audio_file, print_calibration_result, print_transcribe_file_result,
    record_calibration_audio, transcribe_file_event, _calibration_dbfs,
    _calibration_status, _decode_wav, _decode_with_ffmpeg, _mono_float_to_int16,
    _resample_mono,
)
from whisper_dictate.vp_keymap import (  # noqa: E402,F401
    _detect_xkb_layout, _normalize_xkb_layout, _LANG_TO_XKB, _SUPPORTED_XKB_LAYOUTS,
)
from whisper_dictate.vp_capture import CaptureMixin  # noqa: E402,F401
from whisper_dictate.vp_dictate import Dictate, FIRST_AUDIO_WAIT_S  # noqa: E402,F401
from whisper_dictate import vp_dictate  # noqa: E402


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def get_version() -> str:
    here = Path(__file__).resolve().parent
    version_file = here / "VERSION"
    try:
        version = version_file.read_text(encoding="utf-8").strip()
        if version:
            return version.removeprefix("v")
    except OSError:
        pass

    try:
        r = subprocess.run(
            ["git", "describe", "--tags", "--always", "--dirty"],
            cwd=here,
            capture_output=True,
            text=True,
            timeout=1,
        )
        if r.returncode == 0:
            version = r.stdout.strip()
            if version:
                return version.removeprefix("v")
    except Exception:
        pass

    return os.environ.get("VOICEPI_VERSION", "unknown").removeprefix("v")


VERSION = get_version()


# --- Lazy heavy-dependency surface --------------------------------------
# numpy + the transcribe backend (faster_whisper/ctranslate2 via vp_transcribe)
# and the audio DSP helpers in vp_audio are only needed once we actually start
# transcribing. They are exposed via __getattr__ (module-level access) and
# materialised by _load_runtime_modules() right before model load so --help /
# --doctor never import them.
_LAZY_EXPORTS = {
    "whisper_dictate.vp_audio": (
        "MIN_INPUT_DBFS", "MIN_INPUT_SNR_DB", "TARGET_DBFS",
        "_boost_quiet", "_boost_quiet_detail", "_find_arecord_device",
        "_looks_like_speech", "_noise_snr",
    ),
    "whisper_dictate.vp_transcribe": (
        "BEAM_SIZE", "CONTEXT_MIN_SECONDS", "HALLUCINATIONS",
        "INITIAL_PROMPT", "SR", "STT_BACKEND", "TEMPERATURES",
        "VALID_STT_BACKENDS", "_transcribe", "_transcribe_detail",
        "is_hallucination", "load_stt_model",
    ),
}
_EXPORT_ALIASES = {"_HALLUCINATIONS": ("whisper_dictate.vp_transcribe", "HALLUCINATIONS")}


def __getattr__(name: str):
    if name in _EXPORT_ALIASES:
        mod_name, attr = _EXPORT_ALIASES[name]
    else:
        for candidate, names in _LAZY_EXPORTS.items():
            if name in names:
                mod_name, attr = candidate, name
                break
        else:
            raise AttributeError(name)
    module = __import__(mod_name, fromlist=[attr])
    value = getattr(module, attr)
    globals()[name] = value
    return value


def _load_runtime_modules() -> None:
    global np
    global MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS
    global _boost_quiet, _boost_quiet_detail, _find_arecord_device
    global _looks_like_speech, _noise_snr
    global BEAM_SIZE, CONTEXT_MIN_SECONDS, _HALLUCINATIONS, INITIAL_PROMPT
    global SR, STT_BACKEND, TEMPERATURES, VALID_STT_BACKENDS
    global _transcribe, _transcribe_detail, is_hallucination, load_stt_model

    import numpy as np  # noqa: F401
    from whisper_dictate.vp_audio import (
        MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS,
        _boost_quiet, _boost_quiet_detail, _find_arecord_device,
        _looks_like_speech, _noise_snr,
    )
    from whisper_dictate.vp_transcribe import (
        BEAM_SIZE, CONTEXT_MIN_SECONDS,
        HALLUCINATIONS as _HALLUCINATIONS,
        INITIAL_PROMPT, SR, STT_BACKEND, TEMPERATURES, VALID_STT_BACKENDS,
        _transcribe, _transcribe_detail, is_hallucination, load_stt_model,
    )
    # Materialise the same transcribe-side globals inside the Dictate module so
    # its methods (and the unit tests that patch them) resolve correctly.
    vp_dictate._load_runtime_modules()


def _run_utility_subcommands(a, ap) -> None:
    """Handle the one-shot CLI subcommands that don't load an STT model.

    Each branch terminates the process via ``raise SystemExit`` on a hit; if no
    subcommand matches, this returns and ``main`` proceeds to the dictation path.
    """
    if a.doctor:
        raise SystemExit(run_doctor())
    if a.model_capacity:
        if not _print_model_capacity(a.json):
            ap.error("Rust model-capacity helper is not available")
        raise SystemExit(0)
    if a.benchmark_files or a.benchmark_corpus:
        from whisper_dictate.vp_benchmark import run_benchmark
        try:
            run_benchmark(
                a.benchmark_files,
                a.benchmark_backends,
                output_jsonl=a.benchmark_jsonl,
                corpus_manifest=a.benchmark_corpus,
            )
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if a.calibrate_mic is not None or a.calibrate_file:
        try:
            if a.calibrate_file:
                calibrate_file(a.calibrate_file, as_json=a.json)
            else:
                calibrate_microphone(a.calibrate_mic, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if (a.history_list is not None or a.history_last or
            a.history_copy_last or a.history_reinject_last):
        try:
            if a.history_last:
                run_history_command("last", as_json=a.json)
            elif a.history_copy_last:
                run_history_command("copy-last")
            elif a.history_reinject_last:
                run_history_command("reinject-last")
            else:
                run_history_command("list", limit=a.history_list, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)
    if a.post_process_text is not None:
        from whisper_dictate.vp_postprocess import postprocess_text
        result = postprocess_text(a.post_process_text)
        if result.fallback and result.error:
            print(f"[post] fallback: {result.error}", file=sys.stderr, flush=True)
        print(result.text, flush=True)
        raise SystemExit(0)
    if a.dictionary_suggest:
        from whisper_dictate.vp_dictionary_suggest import print_suggestions, suggest_replacements
        try:
            suggestions = suggest_replacements(
                a.dictionary_suggest,
                min_confidence=a.dictionary_suggest_min_confidence,
            )
            print_suggestions(suggestions, as_json=a.json)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            ap.error(str(e))
        raise SystemExit(0)


def _resolve_backend_and_device(a, ap) -> tuple[str, str, str]:
    """Validate VOICEPI_STT_BACKEND and resolve the compute device/type."""
    try:
        backend = STT_BACKEND
        if backend not in VALID_STT_BACKENDS:
            raise ValueError(
                "invalid VOICEPI_STT_BACKEND="
                f"{backend!r}; expected one of {', '.join(VALID_STT_BACKENDS)}")
    except ValueError as e:
        ap.error(str(e))

    if backend == "openai":
        dev, ctype = "api", "remote"
    else:
        try:
            dev, ctype = _resolve_device(a.device)
        except ValueError as e:
            ap.error(str(e))
    return backend, dev, ctype


def _resolve_model_name(a, backend: str) -> tuple[str, str]:
    """Resolve the human label + concrete model name for the chosen backend."""
    label = (
        "NVIDIA Parakeet" if backend == "parakeet"
        else "External API" if backend == "openai"
        else "Whisper"
    )
    loaded_model_name = a.model
    if backend == "parakeet":
        from whisper_dictate.vp_parakeet import resolve_parakeet_model_name
        loaded_model_name = resolve_parakeet_model_name(a.model)
    elif backend == "openai":
        from whisper_dictate.vp_external_api import load_stt_api_settings
        loaded_model_name = load_stt_api_settings(a.model).model
    return label, loaded_model_name


def _load_model(a, ap, backend: str, dev: str, ctype: str) -> tuple[object, str, float]:
    """Load the STT model, emitting the loading_model/ready/failed worker events."""
    label, loaded_model_name = _resolve_model_name(a, backend)
    if backend == "openai":
        print(f"using {label} {loaded_model_name} via configured API", flush=True)
    else:
        print(f"loading {label} {loaded_model_name} on {dev} ({ctype})… "
              f"first run downloads the model", flush=True)
    _emit_worker_event(
        "status",
        state="loading_model",
        backend=backend,
        model=loaded_model_name,
        device=dev,
        compute_type=ctype,
    )
    if dev == "cpu" and backend != "openai":
        print("  note: CPU mode — transcription is slower; large-v3-turbo "
              "(default) is the fastest model", flush=True)
    _t = time.monotonic()
    try:
        _model = load_stt_model(loaded_model_name, dev, ctype)
    except RuntimeError as e:
        message = str(e)
        _emit_worker_event("error", state="failed", backend=backend, model=loaded_model_name, message=message)
        print(f"  x startup error: {message}", flush=True)
        raise SystemExit(1)
    _model_load_s = time.monotonic() - _t
    _emit_worker_event(
        "status",
        state="ready",
        backend=backend,
        model=loaded_model_name,
        device=dev,
        compute_type=ctype,
        model_load_s=round(_model_load_s, 3),
    )
    if backend == "openai":
        print(f"api ready in {_model_load_s:.1f}s", flush=True)
    else:
        print(f"model ready in {_model_load_s:.1f}s", flush=True)
    return _model, loaded_model_name, _model_load_s


def _run_session(a, model, lang, backend: str, dev: str, ctype: str,
                 loaded_model_name: str, model_load_s: float) -> None:
    """Transcribe a one-shot file, or enter the live push-to-talk loop."""
    if a.transcribe_file:
        event = transcribe_file_event(
            model,
            a.transcribe_file,
            lang,
            model_name=loaded_model_name,
            stt_backend=backend,
            device=dev,
            compute_type=ctype,
        )
        print_transcribe_file_result(event, as_json=a.json)
        raise SystemExit(0)
    try:
        Dictate(
            model, a.key, a.mode, lang,
            json_output=a.json,
            metrics_jsonl=os.environ.get("VOICEPI_METRICS_JSONL"),
            model_name=loaded_model_name,
            device=dev,
            compute_type=ctype,
            model_load_s=model_load_s,
        ).run()
    except KeyboardInterrupt:
        print("\nbye")


def main() -> None:
    if not os.environ.get("VOICEPI_LAUNCHER_PRINTED_VERSION"):
        print(f"whisper-dictate {VERSION}", flush=True)
    ap = build_arg_parser()
    a = ap.parse_args()
    apply_config_to_environ()
    _run_utility_subcommands(a, ap)
    lang = None if (a.autodetect or not a.lang) else a.lang

    # Sæt XKB_DEFAULT_LAYOUT fra --lang så ydotool type og evt. auto-startet
    # ydotoold arver det rigtige layout uden manuel konfiguration.
    if lang and not os.environ.get("XKB_DEFAULT_LAYOUT"):
        xkb = _LANG_TO_XKB.get(lang, lang)
        os.environ["XKB_DEFAULT_LAYOUT"] = xkb

    if _apply_local_only_network_lock():
        print("[privacy] local-only mode enabled; cloud backends and model downloads are blocked", flush=True)

    _load_runtime_modules()

    backend, dev, ctype = _resolve_backend_and_device(a, ap)

    if (os.environ.get("VOICEPI_DEBUG") or "").strip().lower() not in (
            "", "0", "false", "no", "off"):
        _print_effective_config(a, dev, ctype)

    model, loaded_model_name, model_load_s = _load_model(a, ap, backend, dev, ctype)
    _run_session(a, model, lang, backend, dev, ctype, loaded_model_name, model_load_s)


if __name__ == "__main__":
    main()
