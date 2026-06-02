"""CLI surface for whisper-dictate: argparse + the VOICEPI_DEBUG settings dump.

Extracted from voice_pi.py so the main module can focus on the runtime
orchestration. All defaults stay env-var-driven (VOICEPI_KEY, VOICEPI_MODEL,
etc.) — the parser only ever sees the resolved value.
"""
from __future__ import annotations

import argparse
import os

from vp_config import apply_config_to_environ, get_value
from vp_device import VALID_DEVICES
from vp_privacy import apply_local_only_network_lock, local_only_enabled
from vp_postprocess import load_postprocess_settings

apply_config_to_environ()
apply_local_only_network_lock()

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


class _DictionaryAction(argparse.Action):
    def __call__(self, parser, namespace, values, option_string=None):
        from vp_dictionary import (
            add_dictionary_replacement, add_dictionary_term,
            dictionary_status, open_dictionary,
        )

        try:
            if option_string == "--dictionary-status":
                print(dictionary_status(), flush=True)
            elif option_string == "--dictionary-open":
                path = open_dictionary()
                print(f"opened dictionary: {path}", flush=True)
            elif option_string == "--dictionary-add":
                path, added = add_dictionary_term(values)
                verb = "added" if added else "already present"
                print(f"{verb}: {values} ({path})", flush=True)
            elif option_string == "--dictionary-replace":
                path, src, dst, changed = add_dictionary_replacement(values)
                verb = "saved" if changed else "already present"
                print(f"{verb}: {src} -> {dst} ({path})", flush=True)
        except Exception as e:  # noqa: BLE001 - argparse should report cleanly
            parser.error(str(e))
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
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--type", action="store_const", dest="mode",
                   const="type",
                   help="force direct keyboard typing; env VOICEPI_INJECT_MODE")
    g.add_argument("--paste", action="store_const", dest="mode",
                   const="paste",
                   help="force clipboard + Ctrl+V on X11/Windows "
                        "(on Wayland direct evdev keycodes are always used)")
    g.add_argument("--no-type", action="store_const", dest="mode",
                   const="print", help="just print, don't inject")
    ap.add_argument("--json", action="store_true", default=_truthy_env("VOICEPI_JSON"),
                    help="also emit one structured JSON event per utterance; "
                         "env VOICEPI_JSON")
    ap.add_argument("--doctor", action="store_true",
                    help="run Linux/Wayland health checks and exit")
    ap.add_argument("--model-capacity", action="store_true",
                    help="show local GPU VRAM and which local models can fit, then exit")
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
                         "for example whisper:large-v3,parakeet")
    ap.add_argument("--benchmark-jsonl", default=None,
                    help="append benchmark JSONL results to this path instead "
                         "of stdout")
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
    ap.add_argument("--dictionary-suggest", metavar="JSONL",
                    help="suggest smart replacements from benchmark/history JSONL, then exit")
    ap.add_argument("--dictionary-suggest-min-confidence", type=float, default=0.62,
                    help="minimum fuzzy-match confidence for --dictionary-suggest")
    ap.add_argument("--device", default=DEVICE, choices=VALID_DEVICES,
                    help="auto|cuda|cpu (default auto; env VOICEPI_DEVICE). "
                         "auto = NVIDIA GPU if present, else CPU")
    ap.set_defaults(mode=INJECT_MODE)
    return ap


def _print_effective_config(args, dev: str, ctype: str) -> None:
    """Dump every setting whisper-dictate honours + the env-var source
    annotation. Triggered by VOICEPI_DEBUG. Useful for "is my setx
    actually arriving?" debugging — print is unconditional and runs
    BEFORE the model loads, so it shows even if the model load hangs."""
    def _env(name: str) -> str:
        v = os.environ.get(name)
        if v is None:
            return "(unset)"
        return v if len(v) <= 60 else f"{v[:57]}..."

    prompt_raw = os.environ.get("VOICEPI_INITIAL_PROMPT") or ""
    if prompt_raw:
        prompt_body = (f"{len(prompt_raw)} chars: \"{prompt_raw[:60]}"
                       f"{'...' if len(prompt_raw) > 60 else ''}\"")
    else:
        prompt_body = "(unset)"
    prompt_preview = f"{prompt_body}  (env VOICEPI_INITIAL_PROMPT)"

    from vp_audio import MIN_INPUT_DBFS, MIN_INPUT_SNR_DB, TARGET_DBFS
    from vp_transcribe import (
        BEAM_SIZE, CONTEXT_MIN_SECONDS, STT_BACKEND, TEMPERATURES,
        VAD_MIN_SILENCE_MS, VAD_THRESHOLD,
    )
    from vp_dictionary import DICTIONARY, _default_path
    post = load_postprocess_settings()

    rows = [
        ("--key",            f"{args.key}  (env VOICEPI_KEY={_env('VOICEPI_KEY')})"),
        ("--model",          f"{args.model}  (env VOICEPI_MODEL={_env('VOICEPI_MODEL')})"),
        ("stt model",        f"{get_value('VOICEPI_STT_MODEL', '(unset)')}  "
                             f"(env VOICEPI_STT_MODEL={_env('VOICEPI_STT_MODEL')})"),
        ("--lang",           f"{(None if (args.autodetect or not args.lang) else args.lang) or 'auto'}  "
                             f"(env VOICEPI_LANG={_env('VOICEPI_LANG')}, "
                             f"--autodetect={args.autodetect})"),
        ("--device",         f"{args.device}  ->  resolved: {dev} / {ctype}"),
        ("stt backend",      f"{STT_BACKEND}  (env VOICEPI_STT_BACKEND={_env('VOICEPI_STT_BACKEND')})"),
        ("stt api",          f"url={get_value('VOICEPI_STT_BASE_URL', '(unset)')} "
                             f"key={'set' if (os.environ.get('VOICEPI_STT_API_KEY') or os.environ.get('OPENAI_API_KEY')) else 'unset'}"),
        ("compute_type",     f"{ctype}  (env VOICEPI_COMPUTE_TYPE={_env('VOICEPI_COMPUTE_TYPE')})"),
        ("beam_size",        f"{BEAM_SIZE}  (env VOICEPI_BEAM_SIZE={_env('VOICEPI_BEAM_SIZE')})"),
        ("temperature",      f"{TEMPERATURES}  (env VOICEPI_TEMPERATURE={_env('VOICEPI_TEMPERATURE')})"),
        ("context_min_s",    f"{CONTEXT_MIN_SECONDS}  (env VOICEPI_CONTEXT_MIN_SECONDS={_env('VOICEPI_CONTEXT_MIN_SECONDS')})"),
        ("parakeet_min_s",   f"{get_value('VOICEPI_PARAKEET_MIN_SECONDS', '1.5')}  "
                             f"(env VOICEPI_PARAKEET_MIN_SECONDS={_env('VOICEPI_PARAKEET_MIN_SECONDS')})"),
        ("release_tail_ms",  f"{get_value('VOICEPI_RELEASE_TAIL_MS', '200')}  "
                             f"(env VOICEPI_RELEASE_TAIL_MS={_env('VOICEPI_RELEASE_TAIL_MS')})"),
        ("vad",              f"threshold={VAD_THRESHOLD}  "
                             f"min_silence_ms={VAD_MIN_SILENCE_MS}"),
        ("initial_prompt",   prompt_preview),
        ("dictionary",       f"{len(DICTIONARY.terms)} terms, "
                             f"{len(DICTIONARY.replacements)} replacements, "
                             f"path={_env('VOICEPI_DICTIONARY') if _env('VOICEPI_DICTIONARY') != '(unset)' else _default_path()}"),
        ("quit",             f"{QUIT_COUNT}x {QUIT_KEY} within {QUIT_WINDOW_MS}ms  "
                             f"(env VOICEPI_QUIT_KEY={_env('VOICEPI_QUIT_KEY')}, "
                             f"VOICEPI_QUIT_COUNT={_env('VOICEPI_QUIT_COUNT')})"),
        ("audio thresholds", f"target_dbfs={TARGET_DBFS}  "
                             f"min_input_dbfs={MIN_INPUT_DBFS}  "
                             f"min_snr_db={MIN_INPUT_SNR_DB}"),
        ("audio ducking",    f"enabled={get_value('VOICEPI_AUDIO_DUCKING', '(unset)')}  "
                             f"level={get_value('VOICEPI_AUDIO_DUCKING_LEVEL', '0.25')}"),
        ("XKB (Wayland)",    f"VOICEPI_XKB_LAYOUT={_env('VOICEPI_XKB_LAYOUT')}  "
                             f"XKB_DEFAULT_LAYOUT={_env('XKB_DEFAULT_LAYOUT')}"),
        ("inject mode",      f"{args.mode}  (env VOICEPI_INJECT_MODE={_env('VOICEPI_INJECT_MODE')})"),
        ("format commands",  f"{get_value('VOICEPI_FORMAT_COMMANDS', 'off')}  "
                             f"(env VOICEPI_FORMAT_COMMANDS={_env('VOICEPI_FORMAT_COMMANDS')})"),
        ("json output",      f"{getattr(args, 'json', False)}  (env VOICEPI_JSON={_env('VOICEPI_JSON')})"),
        ("metrics jsonl",    f"{_env('VOICEPI_METRICS_JSONL')}  (env VOICEPI_METRICS_JSONL)"),
        ("command hook",     f"{_env('VOICEPI_COMMAND_HOOK')}  "
                             f"(timeout_ms={_env('VOICEPI_COMMAND_HOOK_TIMEOUT_MS')})"),
        ("local only",       f"{local_only_enabled()}  (env VOICEPI_LOCAL_ONLY={_env('VOICEPI_LOCAL_ONLY')})"),
        ("post process",     f"{post.processor}/{post.mode} model={post.model} "
                             f"url={post.base_url} timeout_ms={post.timeout_ms}"),
        ("post redaction",   f"enabled={post.redact}  "
                             f"terms={'set' if post.redact_terms else 'unset'}"),
        ("stt debug",        f"{_env('VOICEPI_STT_DEBUG')}  (env VOICEPI_STT_DEBUG)"),
    ]
    print("[debug] effective settings:", flush=True)
    for k, v in rows:
        print(f"  {k:<18} {v}", flush=True)
