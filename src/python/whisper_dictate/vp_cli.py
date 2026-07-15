"""CLI surface for whisper-dictate: argparse + the VOICEPI_DEBUG settings dump.

All defaults stay env-var-driven (VOICEPI_KEY, VOICEPI_MODEL, etc.) so the
parser only ever sees the resolved value.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
from pathlib import Path

from whisper_dictate.vp_config import apply_config_to_environ, get_value
from whisper_dictate.vp_postprocess import load_postprocess_settings

apply_config_to_environ()

VALID_DEVICES = ("auto", "cuda", "cpu")


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def local_only_enabled() -> bool:
    return _truthy(get_value("VOICEPI_LOCAL_ONLY"))


def _apply_local_only_network_lock() -> bool:
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    enabled = local_only_enabled()
    if helper:
        try:
            r = subprocess.run(
                [helper, "privacy"],
                input=json.dumps({"action": "env_updates", "local_only": enabled}),
                text=True,
                encoding="utf-8",
                errors="replace",
                capture_output=True,
                timeout=5,
                shell=False,
            )
            if r.returncode == 0:
                payload = json.loads(r.stdout or "{}")
                if isinstance(payload, dict):
                    for name, value in payload.get("env", {}).items():
                        os.environ.setdefault(str(name), str(value))
                    return bool(payload.get("enabled", False))
        except Exception:
            pass
    if not enabled:
        return False
    for name in (
        "HF_HUB_OFFLINE",
        "TRANSFORMERS_OFFLINE",
        "HF_DATASETS_OFFLINE",
        "HF_HUB_DISABLE_TELEMETRY",
    ):
        os.environ.setdefault(name, "1")
    os.environ.setdefault("WANDB_DISABLED", "true")
    os.environ.setdefault("WANDB_MODE", "offline")
    return True


_apply_local_only_network_lock()

MODEL_NAME = get_value("VOICEPI_MODEL", "large-v3-turbo")
DEVICE = get_value("VOICEPI_DEVICE", "auto")
LANG = get_value("VOICEPI_LANG")  # None -> Whisper auto-detects
KEY = get_value("VOICEPI_KEY", "ctrl_r")

VALID_INJECT_MODES = ("auto", "type", "paste", "print")
INJECT_MODE = (get_value("VOICEPI_INJECT_MODE", "auto") or "auto").strip().lower()
if INJECT_MODE not in VALID_INJECT_MODES:
    INJECT_MODE = "auto"

# Global quit shortcut for the pynput path (Windows/X11). N consecutive
# QUIT_KEY presses within QUIT_WINDOW_MS quit the app. Default 3x Esc — avoids
# accidental shutdown because pynput catches Esc system-wide. Set
# VOICEPI_QUIT_COUNT=0 to disable; 1 = legacy single-Esc behaviour.
QUIT_KEY = (get_value("VOICEPI_QUIT_KEY", "esc") or "esc").strip().lower()
QUIT_COUNT = int(get_value("VOICEPI_QUIT_COUNT", "3") or "3")
QUIT_WINDOW_MS = int(get_value("VOICEPI_QUIT_WINDOW_MS", "1500") or "1500")


def _truthy_env(name: str) -> bool:
    return (os.environ.get(name) or "").strip().lower() not in (
        "", "0", "false", "no", "off")


def _resolve_device(want: str) -> tuple[str, str]:
    want = (want or "auto").lower()
    if want not in VALID_DEVICES:
        raise ValueError(f"invalid device '{want}' (expected: "
                         f"{', '.join(VALID_DEVICES)})")

    override = (get_value("VOICEPI_COMPUTE_TYPE") or "").strip() or None

    def _ct(default: str) -> str:
        return override if override else default

    if want == "cuda":
        return "cuda", _ct("int8_float16")
    if want == "cpu":
        return "cpu", _ct("int8")
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", _ct("int8_float16")
    except Exception:
        pass
    return "cpu", _ct("int8")


def _default_dictionary_path() -> str:
    if os.name == "nt":
        base = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        return str(Path(base) / "WhisperDictate" / "dictionary.json")
    base = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return str(Path(base) / "whisper-dictate" / "dictionary.json")


def _dictionary_path_preview(path: str | None = None) -> str:
    if path:
        return path
    env_path = _env_preview("VOICEPI_DICTIONARY")
    if env_path != "(unset)":
        return env_path
    return _default_dictionary_path()


def _run_rust_dictionary_command(parser: argparse.ArgumentParser, *args: str) -> None:
    helper = os.environ.get("VOICEPI_RUST_INJECTOR")
    if not helper:
        parser.error(
            "dictionary commands are handled by the Rust CLI; "
            "run `whisper-dictate dictionary ...` or start through the Rust launcher"
        )
    try:
        r = subprocess.run(
            [helper, "dictionary", *args],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=10,
            shell=False,
        )
    except Exception as e:  # noqa: BLE001 - argparse should report cleanly
        parser.error(str(e))
    if r.returncode != 0:
        parser.error((r.stderr or "").strip() or "Rust dictionary command failed")
    stdout = r.stdout or ""
    if stdout:
        print(stdout, end="" if stdout.endswith("\n") else "\n", flush=True)


class _DictionaryAction(argparse.Action):
    def __call__(self, parser, namespace, values, option_string=None):
        if option_string == "--dictionary-status":
            _run_rust_dictionary_command(parser, "status")
        elif option_string == "--dictionary-open":
            _run_rust_dictionary_command(parser, "open")
        elif option_string == "--dictionary-add":
            _run_rust_dictionary_command(parser, "add", str(values))
        elif option_string == "--dictionary-replace":
            _run_rust_dictionary_command(parser, "replace", str(values))
        raise SystemExit(0)


def build_arg_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser()
    ap.add_argument("--key", default=KEY,
                    help="pynput Key name held to talk (ctrl_r, alt_r, f9…) "
                         "or chord: shift_r+ctrl_r; env VOICEPI_KEY")
    ap.add_argument("--model", default=MODEL_NAME,
                    help="Whisper model (default large-v3-turbo, fastest; "
                         "env VOICEPI_MODEL)")
    ap.add_argument("--lang", default=LANG,
                    help="spoken-language hint: da, en, de, fr… "
                         "(env VOICEPI_LANG) — omit to let Whisper auto-detect")
    ap.add_argument("--autodetect", action="store_true",
                    help="explicitly auto-detect language (alias for omitting --lang)")
    ap.add_argument("--prompt", default=None, metavar="TEXT",
                    help="override the domain-vocabulary hint seeded into "
                         "Whisper's initial prompt for this run, e.g. "
                         "\"Kubernetes, Proxmox, LiteLLM, ansible\" — wins over "
                         "VOICEPI_INITIAL_PROMPT / the Quality tab's Initial "
                         "prompt (omit to use those); pass \"\" to disable it")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--type", action="store_const", dest="mode",
                   const="type",
                   help="force direct keyboard typing; env VOICEPI_INJECT_MODE")
    g.add_argument("--paste", action="store_const", dest="mode",
                   const="paste",
                   help="force clipboard paste: pyperclip copies the text, "
                        "then a ydotool key shortcut (Ctrl+V or Ctrl+Shift+V) "
                        "triggers the paste on Wayland; on X11/Windows pynput "
                        "sends the Ctrl+V chord")
    g.add_argument("--no-type", action="store_const", dest="mode",
                   const="print", help="just print, don't inject")
    ap.add_argument("--json", action="store_true", default=_truthy_env("VOICEPI_JSON"),
                    help="also emit one structured JSON event per utterance; "
                         "env VOICEPI_JSON")
    ap.add_argument("--doctor", action="store_true",
                    help="run Linux/Wayland health checks and exit")
    ap.add_argument("--model-capacity", action="store_true",
                    help="show local GPU VRAM and which local models can fit, then exit")
    ap.add_argument("--list-audio-devices", action="store_true",
                    help="print available input (microphone) devices as JSON, then exit")
    ap.add_argument("--test-audio-device", metavar="NAME", default=None,
                    help="dry-run open the named microphone (resolve + try the same "
                         "WASAPI/DirectSound/MME open matrix as capture, capturing no "
                         "audio), print a single JSON usability result, then exit. "
                         "Pass an empty string to test the system default input.")
    ap.add_argument("--record-corpus-item", metavar="ID", default=None,
                    help="record reference audio for the golden-corpus item ID from "
                         "the configured microphone (reusing the same negotiated capture "
                         "path as dictation), save it to "
                         "<appdata>/benchmark/audio/<ID>.wav so the benchmark can score "
                         "it, print start/progress/done JSON events, then exit. The "
                         "recording length is derived from the reference text length.")
    ap.add_argument("--list-windows", action="store_true",
                    help="print visible top-level windows (title + process) as JSON, "
                         "then exit. Windows only; exits with code 1 on other platforms.")
    ap.add_argument("--transcribe-file", metavar="PATH",
                    help="transcribe an audio file with the selected backend, "
                         "then exit. 16-bit WAV works natively; mp3/m4a and "
                         "other formats require ffmpeg.")
    ap.add_argument("--benchmark-files", nargs="+", metavar="PATH",
                    help="run one or more audio files through benchmark "
                         "backend specs, then exit")
    ap.add_argument("--benchmark-corpus", metavar="PATH",
                    help="run benchmark entries from a corpus manifest, "
                         "annotating results with reference text, WER/CER "
                         "and term hits")
    ap.add_argument("--benchmark-backends", default=None,
                    help="comma-separated backend specs for benchmark runs, "
                         "for example whisper:large-v3,openai:gpt-4o-mini-transcribe")
    ap.add_argument("--benchmark-jsonl", default=None,
                    help="append benchmark JSONL results to this path instead "
                         "of stdout")
    ap.add_argument("--run-benchmark", action="store_true",
                    help="run the golden corpus (default benchmark/corpus.json, "
                         "overridable via --benchmark-corpus) through the "
                         "configured backend, print per-item JSONL plus one "
                         "[benchmark] summary line, then exit. Drives the "
                         "Settings UI \"Run benchmark\" button.")
    ap.add_argument("--calibrate-mic", nargs="?", const=5.0, type=float,
                    metavar="SECONDS",
                    help="record a short sample, recommend audio threshold "
                         "settings, then exit")
    ap.add_argument("--calibrate-file", metavar="PATH",
                    help="analyze an existing audio file and recommend audio "
                         "threshold settings, then exit")
    ap.add_argument("--post-process-text", metavar="TEXT",
                    help="run the configured local post-processor on TEXT, "
                         "then exit")
    ap.add_argument("--history-list", nargs="?", const=10, type=int,
                    metavar="N",
                    help="print the last N local dictation history entries, "
                         "then exit")
    ap.add_argument("--history-last", action="store_true",
                    help="print the last local dictation transcript, then exit")
    ap.add_argument("--history-copy-last", action="store_true",
                    help="copy the last local dictation transcript to the "
                         "clipboard, then exit")
    ap.add_argument("--history-reinject-last", action="store_true",
                    help="paste the last local dictation transcript into the "
                         "active window, then exit")
    ap.add_argument("--dictionary-status", nargs=0, action=_DictionaryAction,
                    help="show dictionary paths, counts and preview, then exit")
    ap.add_argument("--dictionary-open", nargs=0, action=_DictionaryAction,
                    help="create/open the managed dictionary file, then exit")
    ap.add_argument("--dictionary-add", metavar="TERM", action=_DictionaryAction,
                    help="add TERM to the managed dictionary, then exit")
    ap.add_argument("--dictionary-replace", metavar="FROM=TO",
                    action=_DictionaryAction,
                    help="add a smart replacement to the managed dictionary, then exit")
    ap.add_argument("--setup", action="store_true",
                    help="run the interactive config setup wizard (writes "
                         "config.json + prints env-lines), then exit. Loads no "
                         "ML model.")
    ap.add_argument("--capture-hotkey", action="store_true",
                    help="press-to-capture the push-to-talk hotkey: hold the "
                         "key(s) you want, release, confirm, and it is written to "
                         "the 'key' setting in config.json. No typing key names. "
                         "Then exit; loads no ML model.")
    ap.add_argument("--capture-hotkey-allow-media", action="store_true",
                    help="(experimental) with --capture-hotkey, also capture media "
                         "/ consumer keys (play/pause, volume up/down) that pynput "
                         "exposes as media keys; see issue #258.")
    ap.add_argument("--export-config", action="store_true",
                    help="print the current effective config (config.json + env "
                         "overrides) as a config.json blob plus PowerShell/bash "
                         "env-lines, then exit. Secrets redacted by default.")
    ap.add_argument("--include-secrets", action="store_true",
                    help="with --export-config, emit API keys in full instead of "
                         "redacting them (for backup/migration).")
    ap.add_argument("--dictionary-suggest", metavar="JSONL",
                    help="suggest smart replacements from benchmark/history JSONL, then exit")
    ap.add_argument("--dictionary-suggest-min-confidence", type=float, default=0.62,
                    help="minimum fuzzy-match confidence for --dictionary-suggest")
    ap.add_argument("--dictionary-build-from-corpus", action="store_true",
                    help="extract domain terms from the golden-corpus reference TEXT "
                         "(curated terms + capitalized/multi-word/technical tokens) and "
                         "append+dedup them into the dictionary, then exit. Previews by "
                         "default; pass --apply to write. Reads corpus TEXT only — it "
                         "never records or touches audio. Honours --language/--category.")
    ap.add_argument("--dictionary-suggest-terms", metavar="JSONL",
                    help="read an annotated benchmark JSONL and SUGGEST the domain terms "
                         "the model missed (term_misses) as dictionary additions, then "
                         "exit. Previews by default; pass --apply to add the new terms. "
                         "Reads result TEXT only — never records audio.")
    ap.add_argument("--dictionary", metavar="PATH", default=None,
                    help="dictionary.json to read/append for the training commands "
                         "(default: $VOICEPI_DICTIONARY or the per-user dictionary.json)")
    ap.add_argument("--apply", action="store_true",
                    help="with the dictionary training commands, WRITE the changes "
                         "instead of only previewing them")
    ap.add_argument("--min-count", type=int, default=1,
                    help="minimum corpus/benchmark occurrence count for a term to be "
                         "proposed by the dictionary training commands (default 1)")
    ap.add_argument("--language", default=None,
                    help="corpus profile: restrict the benchmark / dictionary-build to "
                         "these languages, e.g. da or da,en")
    ap.add_argument("--category", default=None,
                    help="corpus profile: restrict to these categories or friendly "
                         "groups (technical, business, names, short, long, ui, …) or an "
                         "exact corpus category; comma-separated")
    ap.add_argument("--device", default=DEVICE, choices=VALID_DEVICES,
                    help="auto|cuda|cpu (default auto; env VOICEPI_DEVICE). "
                         "auto = NVIDIA GPU if present, else CPU")
    ap.add_argument("--app-root", default=None, help=argparse.SUPPRESS)
    # Experimental Rust capture pipeline (audio-in-rust feature). When set,
    # the Rust controller pipes JSON-line audio events into stdin and the
    # capture path reads from there instead of opening sounddevice. The
    # `sounddevice` value is the explicit default and exists so a downstream
    # test or operator can pin it without relying on the absent-flag default.
    # See src/python/whisper_dictate/vp_rust_audio_source.py for the wire
    # format and src/rust/audio/stdin_bridge.rs for the sender. Hidden from
    # the public --help until the rollout completes.
    ap.add_argument("--audio-source", default="sounddevice",
                    choices=["sounddevice", "rust-stdin"],
                    help=argparse.SUPPRESS)
    ap.set_defaults(mode=INJECT_MODE)
    return ap


def _env_preview(name: str) -> str:
    v = os.environ.get(name)
    if v is None:
        return "(unset)"
    return v if len(v) <= 60 else f"{v[:57]}..."


def _initial_prompt_preview() -> str:
    prompt_raw = os.environ.get("VOICEPI_INITIAL_PROMPT") or ""
    if not prompt_raw:
        return "(unset)  (env VOICEPI_INITIAL_PROMPT)"
    suffix = "..." if len(prompt_raw) > 60 else ""
    return f"{len(prompt_raw)} chars: \"{prompt_raw[:60]}{suffix}\"  (env VOICEPI_INITIAL_PROMPT)"


def _api_key_state() -> str:
    return "set" if (
        os.environ.get("VOICEPI_STT_API_KEY")
        or os.environ.get("GROQ_API_KEY")
        or os.environ.get("OPENAI_API_KEY")
    ) else "unset"


def _debug_rows(args, dev: str, ctype: str) -> list[tuple[str, str]]:
    """Return effective settings rows printed before model load."""

    from whisper_dictate.vp_audio import MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS
    from whisper_dictate.vp_transcribe import (
        BEAM_SIZE, CONTEXT_MIN_SECONDS, STT_BACKEND, TEMPERATURES, _dictionary_runtime,
        VAD_MIN_SILENCE_MS, VAD_THRESHOLD,
    )
    dictionary = _dictionary_runtime("", None)
    post = load_postprocess_settings()

    return [
        ("--key",            f"{args.key}  (env VOICEPI_KEY={_env_preview('VOICEPI_KEY')})"),
        ("--model",          f"{args.model}  (env VOICEPI_MODEL={_env_preview('VOICEPI_MODEL')})"),
        ("stt model",        f"{get_value('VOICEPI_STT_MODEL', '(unset)')}  "
                             f"(env VOICEPI_STT_MODEL={_env_preview('VOICEPI_STT_MODEL')})"),
        ("--lang",           f"{(None if (args.autodetect or not args.lang) else args.lang) or 'auto'}  "
                             f"(env VOICEPI_LANG={_env_preview('VOICEPI_LANG')}, "
                             f"--autodetect={args.autodetect})"),
        ("--device",         f"{args.device}  ->  resolved: {dev} / {ctype}"),
        ("stt backend",      f"{STT_BACKEND}  (env VOICEPI_STT_BACKEND={_env_preview('VOICEPI_STT_BACKEND')})"),
        ("stt api",          f"url={get_value('VOICEPI_STT_BASE_URL', '(unset)')} "
                             f"key={_api_key_state()}"),
        ("compute_type",     f"{ctype}  (env VOICEPI_COMPUTE_TYPE={_env_preview('VOICEPI_COMPUTE_TYPE')})"),
        ("beam_size",        f"{BEAM_SIZE}  (env VOICEPI_BEAM_SIZE={_env_preview('VOICEPI_BEAM_SIZE')})"),
        ("temperature",      f"{TEMPERATURES}  (env VOICEPI_TEMPERATURE={_env_preview('VOICEPI_TEMPERATURE')})"),
        ("context_min_s",    f"{CONTEXT_MIN_SECONDS}  (env VOICEPI_CONTEXT_MIN_SECONDS={_env_preview('VOICEPI_CONTEXT_MIN_SECONDS')})"),
        # Wave 8 of #348 removed the parakeet_min_s row together with the
        # backend (no equivalent in the schema any more).
        ("release_tail_ms",  f"{get_value('VOICEPI_RELEASE_TAIL_MS', '200')}  "
                             f"(env VOICEPI_RELEASE_TAIL_MS={_env_preview('VOICEPI_RELEASE_TAIL_MS')})"),
        ("preview_seconds",  f"{get_value('VOICEPI_PREVIEW_SECONDS', '3')}  "
                             f"(env VOICEPI_PREVIEW_SECONDS={_env_preview('VOICEPI_PREVIEW_SECONDS')})"),
        ("vad",              f"threshold={VAD_THRESHOLD}  "
                             f"min_silence_ms={VAD_MIN_SILENCE_MS}"),
        ("initial_prompt",   _initial_prompt_preview()),
        ("dictionary",       f"{dictionary.term_count} terms, "
                             f"{dictionary.replacement_count} replacements, "
                             f"path={_dictionary_path_preview(dictionary.path)}"),
        ("quit",             f"{QUIT_COUNT}x {QUIT_KEY} within {QUIT_WINDOW_MS}ms  "
                             f"(env VOICEPI_QUIT_KEY={_env_preview('VOICEPI_QUIT_KEY')}, "
                             f"VOICEPI_QUIT_COUNT={_env_preview('VOICEPI_QUIT_COUNT')})"),
        ("audio thresholds", f"target_dbfs={TARGET_DBFS}  "
                             f"min_input_dbfs={MIN_INPUT_DBFS}  "
                             f"min_snr_db={MIN_INPUT_SNR_DB}"),
        ("audio ducking",    f"enabled={get_value('VOICEPI_AUDIO_DUCKING', '(unset)')}  "
                             f"level={get_value('VOICEPI_AUDIO_DUCKING_LEVEL', '0.25')}"),
        ("XKB (Wayland)",    f"VOICEPI_XKB_LAYOUT={_env_preview('VOICEPI_XKB_LAYOUT')}  "
                             f"XKB_DEFAULT_LAYOUT={_env_preview('XKB_DEFAULT_LAYOUT')}"),
        ("inject mode",      f"{args.mode}  (env VOICEPI_INJECT_MODE={_env_preview('VOICEPI_INJECT_MODE')})"),
        ("format commands",  f"{get_value('VOICEPI_FORMAT_COMMANDS', 'off')}  "
                             f"(env VOICEPI_FORMAT_COMMANDS={_env_preview('VOICEPI_FORMAT_COMMANDS')})"),
        ("json output",      f"{getattr(args, 'json', False)}  (env VOICEPI_JSON={_env_preview('VOICEPI_JSON')})"),
        ("metrics jsonl",    f"{_env_preview('VOICEPI_METRICS_JSONL')}  (env VOICEPI_METRICS_JSONL)"),
        ("command hook",     f"{_env_preview('VOICEPI_COMMAND_HOOK')}  "
                             f"(timeout_ms={_env_preview('VOICEPI_COMMAND_HOOK_TIMEOUT_MS')})"),
        ("local only",       f"{local_only_enabled()}  (env VOICEPI_LOCAL_ONLY={_env_preview('VOICEPI_LOCAL_ONLY')})"),
        ("post process",     f"{post.processor}/{post.mode} model={post.model} "
                             f"url={post.base_url} timeout_ms={post.timeout_ms}"),
        ("post redaction",   f"enabled={post.redact}  "
                             f"terms={'set' if post.redact_terms else 'unset'}"),
        ("stt debug",        f"{_env_preview('VOICEPI_STT_DEBUG')}  (env VOICEPI_STT_DEBUG)"),
        ("trace",            f"{_env_preview('VOICEPI_TRACE')}  (env VOICEPI_TRACE)"),
    ]


def _print_effective_config(args, dev: str, ctype: str) -> None:
    """Dump every setting whisper-dictate honours before the model loads."""
    print("[debug] effective settings:", flush=True)
    for k, v in _debug_rows(args, dev, ctype):
        print(f"  {k:<18} {v}", flush=True)
