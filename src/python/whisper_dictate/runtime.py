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
    appdata_dir, apply_config_to_environ, config_mtime, effective_config, get_value, load_config,  # noqa: F401
)
from whisper_dictate.vp_doctor import (  # noqa: E402,F401
    Check, run_doctor, _base_checks, _linux_checks, _print_checks,
    _print_fix_hints, _in_group, _can_import, _event_devices_readable,
    _ydotoold_process_detail,
)
from whisper_dictate.vp_audio_ducking import (  # noqa: E402,F401
    AudioDucker, register_active_ducker, restore_all_duckers,
)
from whisper_dictate.vp_rust import _rust_helper, _rust_json, no_console_window_kwargs  # noqa: E402,F401
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
    _sounddevice_stream_kwargs, list_input_devices, print_audio_devices,
    print_windows,
    SOUNDDEVICE_START_BLOCK_MS,
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
from whisper_dictate.vp_capture import (  # noqa: E402,F401
    CaptureMixin, resolve_startup_audio_device, trace_dump_audio_devices,
)
from whisper_dictate.vp_dictate import Dictate, FIRST_AUDIO_WAIT_S  # noqa: E402,F401
from whisper_dictate import vp_dictate  # noqa: E402
from whisper_dictate import vp_dictate_engine  # noqa: E402
from whisper_dictate.vp_dictate_engine import (  # noqa: E402,F401
    ENGINE_ENV, ENGINE_PYTHON, ENGINE_RUST,
    is_known_engine, run_rust_engine, select_engine,
)
from whisper_dictate.vp_feedback import notify_error  # noqa: E402


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _config_dump_enabled() -> bool:
    """Whether to print the startup ``[debug] effective settings:`` dump.

    Moved from Basic to Verbose: Basic (debug:on, stt_debug:off) now shows the
    concise per-utterance ``[health]`` line instead, so the heavy config dump
    requires BOTH VOICEPI_DEBUG and VOICEPI_STT_DEBUG (i.e. Verbose).
    """
    return _truthy(os.environ.get("VOICEPI_DEBUG")) and _truthy(
        os.environ.get("VOICEPI_STT_DEBUG"))


def _trace_enabled() -> bool:
    """Whether maximal ``Trace`` diagnostics are on (env ``VOICEPI_TRACE``).

    Trace is purely additive on top of Verbose: it gates the startup
    full-audio-device enumeration dump and the per-attempt capture logging in
    vp_capture, so normal users never see the high-volume ``[trace]`` lines.
    """
    return _truthy(os.environ.get("VOICEPI_TRACE"))


def _version_from_files(start: Path) -> str | None:
    """Find the nearest VERSION file at or above ``start`` (max 5 levels up).

    The VERSION file ships at the repo root (dev checkout) or the bundle/app root
    (installed), a few directories above this package — not in the package dir
    itself — so we walk up instead of only checking ``start``.
    """
    for base in (start, *list(start.parents)[:5]):
        try:
            version = (base / "VERSION").read_text(encoding="utf-8").strip()
        except OSError:
            continue
        if version:
            return version.removeprefix("v")
    return None


def get_version() -> str:
    here = Path(__file__).resolve().parent
    found = _version_from_files(here)
    if found:
        return found

    try:
        r = subprocess.run(
            ["git", "describe", "--tags", "--always", "--dirty"],
            cwd=here,
            capture_output=True,
            text=True,
            encoding="utf-8",
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
        "_looks_like_speech", "_noise_snr", "_trim_trailing_silence",
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
    global _looks_like_speech, _noise_snr, _trim_trailing_silence
    global BEAM_SIZE, CONTEXT_MIN_SECONDS, _HALLUCINATIONS, INITIAL_PROMPT
    global SR, STT_BACKEND, TEMPERATURES, VALID_STT_BACKENDS
    global _transcribe, _transcribe_detail, is_hallucination, load_stt_model

    import numpy as np  # noqa: F401
    from whisper_dictate.vp_audio import (
        MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS,
        _boost_quiet, _boost_quiet_detail, _find_arecord_device,
        _looks_like_speech, _noise_snr, _trim_trailing_silence,
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


def _handle_model_capacity(a, ap) -> None:
    if not _print_model_capacity(a.json):
        ap.error("Rust model-capacity helper is not available")


def _benchmark_profile(a):
    """Build the corpus profile from --language/--category, or None when unset.

    Returns None when neither selector is given so the benchmark keeps its default
    (all corpus items); otherwise a CorpusProfile that filters the corpus subset.
    """
    if not getattr(a, "language", None) and not getattr(a, "category", None):
        return None
    from whisper_dictate.vp_corpus_profile import build_profile
    return build_profile(language=a.language, category=a.category)


def _handle_benchmark(a, ap) -> None:
    from whisper_dictate.vp_benchmark import run_benchmark, run_corpus_benchmark
    profile = _benchmark_profile(a)
    try:
        if getattr(a, "run_benchmark", False):
            # The UI "Run benchmark" button drives this: the corpus is resolved
            # relative to --app-root (so it works in the installed app, not just a
            # dev checkout), configured backend, per-item JSONL + one [benchmark]
            # summary line on stdout.
            run_corpus_benchmark(
                a.benchmark_corpus,
                a.benchmark_backends,
                output_jsonl=a.benchmark_jsonl,
                app_root=getattr(a, "app_root", None),
                profile=profile,
            )
        else:
            run_benchmark(
                a.benchmark_files,
                a.benchmark_backends,
                output_jsonl=a.benchmark_jsonl,
                corpus_manifest=a.benchmark_corpus,
                appdata=appdata_dir(),
                profile=profile,
            )
    except Exception as e:  # noqa: BLE001 - argparse should report cleanly
        ap.error(str(e))


def _handle_calibrate(a, ap) -> None:
    try:
        if a.calibrate_file:
            calibrate_file(a.calibrate_file, as_json=a.json)
        else:
            calibrate_microphone(a.calibrate_mic, as_json=a.json)
    except Exception as e:  # noqa: BLE001 - argparse should report cleanly
        ap.error(str(e))


def _handle_history(a, ap) -> None:
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


def _handle_post_process(a) -> None:
    from whisper_dictate.vp_postprocess import postprocess_text
    result = postprocess_text(a.post_process_text)
    if result.fallback and result.error:
        print(f"[post] fallback: {result.error}", file=sys.stderr, flush=True)
    print(result.text, flush=True)


def _handle_dictionary_suggest(a, ap) -> int:
    """Shell out to ``whisper-dictate dictionary suggest-replacements``.

    Audit item 4 (`docs/architecture-audit-2026-07-16.md`): the Python
    fuzzy-replacement suggester (`vp_dictionary_suggest.py`) was retired
    once the Rust `dictionary/suggest` port shipped as the shipping
    implementation. The Python `--dictionary-suggest` flag now delegates to
    the Rust subcommand, which owns the loader, matcher and preview
    formatter. The Rust binary MUST be available (`VOICEPI_RUST_INJECTOR`);
    without it we fail cleanly rather than silently do nothing.
    """
    args = ["suggest-replacements", str(a.dictionary_suggest)]
    if getattr(a, "dictionary_suggest_min_confidence", None) is not None:
        args.extend(["--min-confidence", str(a.dictionary_suggest_min_confidence)])
    if a.dictionary is not None:
        args.extend(["--dictionary", str(a.dictionary)])
    if a.json:
        args.append("--json")
    rc = _rust_dictionary_subcommand(args)
    if rc is None:
        ap.error(
            "the Rust `whisper-dictate` binary is required for "
            "--dictionary-suggest (VOICEPI_RUST_INJECTOR unset or points at a "
            "missing helper). Install/repair the app or run "
            "`whisper-dictate dictionary suggest-replacements` directly."
        )
    return rc


def _handle_dictionary_training(a, ap) -> int:
    """Dispatch the two corpus->dictionary training commands (Feature A).

    Both `--dictionary-build-from-corpus` and `--dictionary-suggest-terms`
    shell out to the Rust binary's `dictionary build-from-corpus` /
    `dictionary suggest-terms` subcommands. Audit item 4 (`docs/architecture-
    audit-2026-07-16.md`) retired the in-process Python parity — the Rust
    subcommands are the sole implementation now, so a missing binary is a
    hard error (previous versions silently fell back).
    """
    args = _rust_dictionary_training_args(a)
    if args is None:
        # Defensive: the dispatcher should only call this when one of the
        # recognised flags is set — surface a clear error rather than an
        # empty argv.
        ap.error(
            "no dictionary training flag set — expected "
            "--dictionary-build-from-corpus or --dictionary-suggest-terms"
        )
    rc = _rust_dictionary_subcommand(args)
    if rc is None:
        ap.error(
            "the Rust `whisper-dictate` binary is required for "
            "--dictionary-build-from-corpus / --dictionary-suggest-terms "
            "(VOICEPI_RUST_INJECTOR unset or points at a missing helper). "
            "Install/repair the app or run `whisper-dictate dictionary "
            "build-from-corpus` / `dictionary suggest-terms` directly."
        )
    return rc


def _rust_dictionary_subcommand(args: list[str]) -> int | None:
    """Shell out to ``whisper-dictate dictionary <args>``.

    Returns the subprocess exit code on success, or ``None`` when the Rust
    binary is unavailable (env var unset OR the helper file is missing OR
    the subprocess itself failed to launch). Never falls back to Python —
    audit item 4 retired the parity code; callers surface a clear error.
    """
    helper = os.environ.get("VOICEPI_RUST_INJECTOR") or ""
    if not helper:
        return None
    if not Path(helper).exists():
        return None
    try:
        result = subprocess.run(
            [helper, "dictionary", *args],
            text=True,
            encoding="utf-8",
            errors="replace",
            shell=False,
            **no_console_window_kwargs(),
        )
    except Exception as exc:  # noqa: BLE001 - launch failure surfaces via caller
        print(f"[rust:dictionary] {exc}", file=sys.stderr, flush=True)
        return None
    return result.returncode


def _rust_dictionary_training_args(a) -> list[str] | None:
    """Translate the training argparse flags into the Rust subcommand argv.

    Returns ``None`` when no recognised training flag is present so the
    dispatcher can surface a clean error rather than run an empty argv.
    """
    if getattr(a, "dictionary_suggest_terms", None):
        args = ["suggest-terms", str(a.dictionary_suggest_terms)]
        if a.dictionary is not None:
            args.extend(["--dictionary", str(a.dictionary)])
        if a.min_count and a.min_count != 1:
            args.extend(["--min-count", str(a.min_count)])
        if a.apply:
            args.append("--apply")
        if a.json:
            args.append("--json")
        return args
    if getattr(a, "dictionary_build_from_corpus", False):
        args = ["build-from-corpus"]
        if a.benchmark_corpus:
            args.extend(["--benchmark-corpus", str(a.benchmark_corpus)])
        app_root = getattr(a, "app_root", None)
        if app_root:
            args.extend(["--app-root", str(app_root)])
        if a.dictionary is not None:
            args.extend(["--dictionary", str(a.dictionary)])
        if a.language:
            args.extend(["--language", str(a.language)])
        if a.category:
            args.extend(["--category", str(a.category)])
        if a.min_count and a.min_count != 1:
            args.extend(["--min-count", str(a.min_count)])
        if a.apply:
            args.append("--apply")
        if a.json:
            args.append("--json")
        return args
    return None


def _run_utility_subcommands(a, ap) -> None:
    """Handle the one-shot CLI subcommands that don't load an STT model.

    Each branch terminates the process via ``raise SystemExit`` on a hit; if no
    subcommand matches, this returns and ``main`` proceeds to the dictation path.
    The per-subcommand work lives in ``_handle_*`` helpers to keep this dispatch
    flat.
    """
    if getattr(a, "include_secrets", False) and not getattr(a, "export_config", False):
        ap.error("--include-secrets requires --export-config")
    if (getattr(a, "capture_hotkey_allow_media", False)
            and not getattr(a, "capture_hotkey", False)):
        ap.error("--capture-hotkey-allow-media requires --capture-hotkey")
    if getattr(a, "setup", False):
        from whisper_dictate.vp_setup import run_setup
        raise SystemExit(run_setup())
    if getattr(a, "capture_hotkey", False):
        from whisper_dictate.vp_keys_capture_cli import run_capture_hotkey
        raise SystemExit(run_capture_hotkey(
            allow_media=getattr(a, "capture_hotkey_allow_media", False)))
    if getattr(a, "export_config", False):
        from whisper_dictate.vp_setup import run_export
        raise SystemExit(run_export(include_secrets=getattr(a, "include_secrets", False)))
    if a.doctor:
        raise SystemExit(run_doctor())
    if a.list_audio_devices:
        raise SystemExit(print_audio_devices())
    if a.test_audio_device is not None:
        from whisper_dictate.vp_device_test import test_audio_device
        raise SystemExit(test_audio_device(a.test_audio_device))
    if getattr(a, "record_corpus_item", None) is not None:
        from whisper_dictate.vp_corpus_record import record_corpus_item
        # Same corpus resolution as --run-benchmark (--app-root → appdata) and the
        # same per-user audio dir the benchmark already reads from.
        raise SystemExit(record_corpus_item(
            a.record_corpus_item,
            app_root=getattr(a, "app_root", None),
            appdata=appdata_dir(),
        ))
    if a.list_windows:
        raise SystemExit(print_windows())
    if a.model_capacity:
        _handle_model_capacity(a, ap)
        raise SystemExit(0)
    if a.benchmark_files or a.benchmark_corpus or getattr(a, "run_benchmark", False):
        _handle_benchmark(a, ap)
        raise SystemExit(0)
    if a.calibrate_mic is not None or a.calibrate_file:
        _handle_calibrate(a, ap)
        raise SystemExit(0)
    if (a.history_list is not None or a.history_last or
            a.history_copy_last or a.history_reinject_last):
        _handle_history(a, ap)
        raise SystemExit(0)
    if a.post_process_text is not None:
        _handle_post_process(a)
        raise SystemExit(0)
    if a.dictionary_suggest:
        raise SystemExit(_handle_dictionary_suggest(a, ap))
    if getattr(a, "dictionary_build_from_corpus", False) or getattr(a, "dictionary_suggest_terms", None):
        raise SystemExit(_handle_dictionary_training(a, ap))


def _resolve_backend_and_device(a, ap) -> tuple[str, str, str]:
    """Validate VOICEPI_STT_BACKEND and resolve the compute device/type."""
    try:
        backend = STT_BACKEND
        backend = _validate_backend_opt_rust(backend)
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


def _validate_backend_opt_rust(backend: str) -> str:
    """Validate the STT backend, with optional shell-out to the Rust helper.

    Default (gate off): the Python ``VALID_STT_BACKENDS`` membership check
    runs unchanged. With ``VOICEPI_DICTATE_BACKEND=rust`` AND a resolvable
    Rust helper, the validation is delegated to ``whisper-dictate
    dictate-ops`` (Wave 5 shell-out for the dictation orchestrator pure
    helpers). The Rust path raises ``ValueError`` on an unknown backend
    with the SAME ``invalid VOICEPI_STT_BACKEND=...`` text so ``ap.error``
    surfaces an identical message either way.
    """
    from whisper_dictate.vp_dictate_rust import rust_validate_backend
    rust_result = None
    try:
        rust_result = rust_validate_backend(backend)
    except ValueError:
        raise
    if rust_result is not None:
        canonical, _label = rust_result
        return canonical
    # Pure-Python membership check. We DELIBERATELY avoid importing
    # vp_transcribe here: importing it materialises the ML stack (numpy
    # + faster_whisper) at module-import time, which would break the
    # runtime module's lazy-dependency contract and make this helper
    # unusable from the lightweight `--help` / unit-test paths that run
    # before `_load_runtime_modules()` has materialised the lazy global.
    # When the lazy global IS already populated (post-startup), we use
    # it; otherwise we fall back to a local copy of the small tuple. The
    # `globals().get` lookup does NOT trigger `__getattr__`, so a cold
    # import path stays import-free. The canonical definition still
    # lives in vp_transcribe (see VALID_STT_BACKENDS there); the two
    # are kept in sync by `test_valid_backends_local_copy_matches_canonical`.
    _VALID = globals().get("VALID_STT_BACKENDS") or _VALID_STT_BACKENDS_LOCAL
    if backend not in _VALID:
        raise ValueError(
            "invalid VOICEPI_STT_BACKEND="
            f"{backend!r}; expected one of {', '.join(_VALID)}")
    return backend


# Local mirror of `vp_transcribe.VALID_STT_BACKENDS`, used by the lightweight
# validation path above so that callers running before `_load_runtime_modules()`
# (e.g. unit tests, `--help`) don't drag the heavy ML stack in for a pure
# membership check. A dedicated unit test keeps the two definitions in sync.
# Wave 8 of #348 dropped the `"parakeet"` entry together with the backend.
_VALID_STT_BACKENDS_LOCAL: tuple[str, ...] = ("whisper", "openai")


def _resolve_model_name(a, backend: str) -> tuple[str, str]:
    """Resolve the human label + concrete model name for the chosen backend."""
    label = _backend_label_opt_rust(backend)
    loaded_model_name = a.model
    if backend == "openai":
        from whisper_dictate.vp_external_api import load_stt_api_settings
        loaded_model_name = load_stt_api_settings(a.model).model
    return label, loaded_model_name


def _backend_label_opt_rust(backend: str) -> str:
    """Return the human label for ``backend``, optionally via the Rust helper.

    Mirrors the Python fall-through (OpenAI/Whisper) byte-for-byte when the
    gate is off. With ``VOICEPI_DICTATE_BACKEND=rust`` the label comes from
    ``whisper-dictate dictate-ops`` so the canonical mapping lives in one
    place — the Rust dictate module — ready for Wave 8. The NVIDIA Parakeet
    backend was removed in Wave 8 of #348.
    """
    from whisper_dictate.vp_dictate_rust import rust_validate_backend
    try:
        rust_result = rust_validate_backend(backend)
    except ValueError:
        rust_result = None
    if rust_result is not None:
        return rust_result[1]
    if backend == "openai":
        return "External API"
    return "Whisper"


def _load_model(a, backend: str, dev: str, ctype: str) -> tuple[object, str, float]:
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
        notify_error("whisper-dictate", f"Model load failed: {message}")
        raise SystemExit(1)
    _model_load_s = time.monotonic() - _t
    # Trace: log the FULL audio-device enumeration once at startup so a mic that
    # won't open is diagnosable from the log alone (every capture attempt below
    # can be cross-referenced against which devices/host-APIs even exist). Gated
    # on VOICEPI_TRACE; the dump itself never raises / never blocks startup.
    if _trace_enabled():
        trace_dump_audio_devices()
    # Resolve the active input device WITHOUT opening a stream so the UI can show
    # the microphone from `ready` (not blank "Input pending" until the first
    # recording opens the capture stream). Degrades to "System default" on any
    # failure; the truly-bound device is re-derived when the first recording opens
    # the stream, so a default label is corrected if reality differs.
    audio_device = resolve_startup_audio_device()
    _emit_worker_event(
        "status",
        state="ready",
        backend=backend,
        model=loaded_model_name,
        device=dev,
        compute_type=ctype,
        model_load_s=round(_model_load_s, 3),
        audio_device=audio_device,
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
    if getattr(a, "simulate_ptt", False):
        # Library-first POC: drive the full PTT pipeline against a WAV file.
        # Model is already loaded above; hand it to the simulator and exit.
        from whisper_dictate.vp_simulate_ptt import (
            _print_result, simulate_ptt,
        )
        if not a.wav:
            raise SystemExit("--simulate-ptt requires --wav PATH")
        result = simulate_ptt(
            model,
            a.wav,
            lang=lang,
            inject=bool(a.inject),
        )
        _print_result(result, as_json=a.json)
        raise SystemExit(0)
    try:
        _dispatch_engine(
            a, model, lang, backend, dev, ctype,
            loaded_model_name, model_load_s,
        )
    except KeyboardInterrupt:
        print("\nbye")


def _dispatch_engine(a, model, lang, backend: str, dev: str, ctype: str,
                     loaded_model_name: str, model_load_s: float) -> None:
    """Pick the engine that runs the push-to-talk loop.

    Reads ``VOICEPI_DICTATE_ENGINE`` and — when set to ``rust`` — shells
    out to ``whisper-dictate dictate-run --json-events`` (audit item 5
    Phase A step 2, see docs/design/item5-wire-dictate-session.md).
    Anything else (unset, empty, ``python``, or an unknown value) runs
    the in-process ``Dictate(...).run()`` loop unchanged.

    Rust engine failures at start-up (binary missing, features not
    compiled in, spawn error, early crash before the READY signal) are
    surfaced as a log line and then fall back to the Python engine. The
    opt-in must never take down the worker.

    Split from ``_run_session`` so tests can drive the dispatch decision
    without stubbing out the whole session function's argparse surface.
    """
    raw = os.environ.get(vp_dictate_engine.ENGINE_ENV)
    engine = vp_dictate_engine.select_engine()
    if not vp_dictate_engine.is_known_engine(raw):
        print(
            f"[runtime] Unknown {vp_dictate_engine.ENGINE_ENV}={raw!r} "
            "— falling back to python engine",
            file=sys.stderr,
            flush=True,
        )
        engine = vp_dictate_engine.ENGINE_PYTHON

    if engine == vp_dictate_engine.ENGINE_RUST:
        ran, code = vp_dictate_engine.run_rust_engine(
            config_path=os.environ.get("VOICEPI_CONFIG"),
        )
        if ran:
            raise SystemExit(code if isinstance(code, int) else 0)
        # else: fall through to the Python engine so a failed opt-in is
        # never a total worker failure.

    Dictate(
        model, a.key, a.mode, lang,
        json_output=a.json,
        metrics_jsonl=os.environ.get("VOICEPI_METRICS_JSONL"),
        model_name=loaded_model_name,
        device=dev,
        compute_type=ctype,
        model_load_s=model_load_s,
        # See vp_cli.build_arg_parser: defaults to "sounddevice".
        audio_source=getattr(a, "audio_source", "sounddevice"),
    ).run()


def _force_initial_prompt(prompt: str | None) -> None:
    """Force the resolved ``--prompt`` onto the initial-prompt globals.

    The transcribe module reads ``INITIAL_PROMPT`` (its own module global, also
    set live by the config-reload path) and ``runtime`` re-exports a copy via
    ``_load_runtime_modules``. Both are captured through ``get_value()``, which
    reads CONFIG before env, so neither an env export nor argparse alone lets the
    flag win over a saved ``initial_prompt`` setting. Update BOTH globals (kept in
    sync) so the flag wins for this run. An empty string clears the prompt.

    Also re-export the value to ``VOICEPI_INITIAL_PROMPT``: several modules call
    ``apply_config_to_environ()`` at import (vp_audio/vp_transcribe/...), so the
    early env set in ``main`` is clobbered back to the config value while loading
    runtime modules. Re-syncing here — AFTER those imports — keeps the env (read
    directly by the debug dump / ``--show-config``) consistent with the globals.

    Finally set ``INITIAL_PROMPT_FORCED`` so the live config reload leaves the
    prompt alone: the CLI flag stays authoritative for the whole session rather
    than being overwritten by the saved config value on the next reload (#154).

    ``None`` means "no override" and is a no-op (nothing is forced); an explicit
    empty string ``""`` is a deliberate "disable the prompt for this run" and IS
    forced. Only call this when ``--prompt`` was actually given.
    """
    if prompt is None:
        return
    global INITIAL_PROMPT
    INITIAL_PROMPT = prompt or None
    os.environ["VOICEPI_INITIAL_PROMPT"] = prompt
    from whisper_dictate import vp_transcribe
    vp_transcribe.INITIAL_PROMPT = INITIAL_PROMPT
    vp_transcribe.INITIAL_PROMPT_FORCED = True


def main() -> None:
    if not os.environ.get("VOICEPI_LAUNCHER_PRINTED_VERSION"):
        print(f"whisper-dictate {VERSION}", flush=True)
    ap = build_arg_parser()
    a = ap.parse_args()
    apply_config_to_environ()
    # --prompt overrides VOICEPI_INITIAL_PROMPT / the saved Initial-prompt setting
    # for THIS run (like --lang). Set the env now — AFTER apply_config_to_environ
    # so config can't clobber it — so --show-config and the debug dump (which read
    # the env directly) reflect the flag. The transcribe module's resolved global
    # is refreshed below (get_value reads config first, so env alone wouldn't win).
    if a.prompt is not None:
        os.environ["VOICEPI_INITIAL_PROMPT"] = a.prompt
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

    # The transcribe module captured INITIAL_PROMPT from get_value() at import,
    # which reads config BEFORE env — so refresh it from the resolved --prompt to
    # guarantee the flag wins over a saved config value for this run.
    if a.prompt is not None:
        _force_initial_prompt(a.prompt)

    backend, dev, ctype = _resolve_backend_and_device(a, ap)

    if _config_dump_enabled():
        _print_effective_config(a, dev, ctype)

    model, loaded_model_name, model_load_s = _load_model(a, backend, dev, ctype)
    _run_session(a, model, lang, backend, dev, ctype, loaded_model_name, model_load_s)


if __name__ == "__main__":
    main()
