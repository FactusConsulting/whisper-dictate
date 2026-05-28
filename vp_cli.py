"""CLI surface for whisper-dictate: argparse + the VOICEPI_DEBUG settings dump.

Extracted from voice_pi.py so the main module can focus on the runtime
orchestration. All defaults stay env-var-driven (VOICEPI_KEY, VOICEPI_MODEL,
etc.) — the parser only ever sees the resolved value.
"""
from __future__ import annotations

import argparse
import os

from vp_device import VALID_DEVICES

MODEL_NAME = os.environ.get("VOICEPI_MODEL", "large-v3-turbo")
DEVICE = os.environ.get("VOICEPI_DEVICE", "auto")
LANG = os.environ.get("VOICEPI_LANG")  # None -> Whisper auto-detects
KEY = os.environ.get("VOICEPI_KEY", "ctrl_r")

VALID_INJECT_MODES = ("auto", "type", "paste", "print")
INJECT_MODE = (os.environ.get("VOICEPI_INJECT_MODE") or "auto").strip().lower()
if INJECT_MODE not in VALID_INJECT_MODES:
    INJECT_MODE = "auto"

# Global quit shortcut for the pynput path (Windows/X11). N consecutive
# Esc presses within QUIT_WINDOW_MS quit the app. Default 3 — avoids
# accidental shutdown because pynput catches Esc system-wide. Set
# VOICEPI_QUIT_COUNT=0 to disable; 1 = legacy single-Esc behaviour.
QUIT_COUNT = int(os.environ.get("VOICEPI_QUIT_COUNT", "3"))
QUIT_WINDOW_MS = int(os.environ.get("VOICEPI_QUIT_WINDOW_MS", "1500"))


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
    ap.add_argument("--dictionary-status", nargs=0, action=_DictionaryAction,
                    help="show dictionary paths, counts and preview, then exit")
    ap.add_argument("--dictionary-open", nargs=0, action=_DictionaryAction,
                    help="create/open the managed dictionary file, then exit")
    ap.add_argument("--dictionary-add", metavar="TERM", action=_DictionaryAction,
                    help="add TERM to the managed dictionary, then exit")
    ap.add_argument("--dictionary-replace", metavar="FROM=TO",
                    action=_DictionaryAction,
                    help="add a smart replacement to the managed dictionary, then exit")
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

    rows = [
        ("--key",            f"{args.key}  (env VOICEPI_KEY={_env('VOICEPI_KEY')})"),
        ("--model",          f"{args.model}  (env VOICEPI_MODEL={_env('VOICEPI_MODEL')})"),
        ("--lang",           f"{(None if (args.autodetect or not args.lang) else args.lang) or 'auto'}  "
                             f"(env VOICEPI_LANG={_env('VOICEPI_LANG')}, "
                             f"--autodetect={args.autodetect})"),
        ("--device",         f"{args.device}  ->  resolved: {dev} / {ctype}"),
        ("stt backend",      f"{STT_BACKEND}  (env VOICEPI_STT_BACKEND={_env('VOICEPI_STT_BACKEND')})"),
        ("compute_type",     f"{ctype}  (env VOICEPI_COMPUTE_TYPE={_env('VOICEPI_COMPUTE_TYPE')})"),
        ("beam_size",        f"{BEAM_SIZE}  (env VOICEPI_BEAM_SIZE={_env('VOICEPI_BEAM_SIZE')})"),
        ("temperature",      f"{TEMPERATURES}  (env VOICEPI_TEMPERATURE={_env('VOICEPI_TEMPERATURE')})"),
        ("context_min_s",    f"{CONTEXT_MIN_SECONDS}  (env VOICEPI_CONTEXT_MIN_SECONDS={_env('VOICEPI_CONTEXT_MIN_SECONDS')})"),
        ("vad",              f"threshold={VAD_THRESHOLD}  "
                             f"min_silence_ms={VAD_MIN_SILENCE_MS}"),
        ("initial_prompt",   prompt_preview),
        ("dictionary",       f"{len(DICTIONARY.terms)} terms, "
                             f"{len(DICTIONARY.replacements)} replacements, "
                             f"path={_env('VOICEPI_DICTIONARY') if _env('VOICEPI_DICTIONARY') != '(unset)' else _default_path()}"),
        ("quit",             f"{QUIT_COUNT}x Esc within {QUIT_WINDOW_MS}ms  "
                             f"(env VOICEPI_QUIT_COUNT={_env('VOICEPI_QUIT_COUNT')})"),
        ("audio thresholds", f"target_dbfs={TARGET_DBFS}  "
                             f"min_input_dbfs={MIN_INPUT_DBFS}  "
                             f"min_snr_db={MIN_INPUT_SNR_DB}"),
        ("XKB (Wayland)",    f"VOICEPI_XKB_LAYOUT={_env('VOICEPI_XKB_LAYOUT')}  "
                             f"XKB_DEFAULT_LAYOUT={_env('XKB_DEFAULT_LAYOUT')}"),
        ("inject mode",      f"{args.mode}  (env VOICEPI_INJECT_MODE={_env('VOICEPI_INJECT_MODE')})"),
        ("json output",      f"{getattr(args, 'json', False)}  (env VOICEPI_JSON={_env('VOICEPI_JSON')})"),
        ("metrics jsonl",    f"{_env('VOICEPI_METRICS_JSONL')}  (env VOICEPI_METRICS_JSONL)"),
        ("stt debug",        f"{_env('VOICEPI_STT_DEBUG')}  (env VOICEPI_STT_DEBUG)"),
    ]
    print("[debug] effective settings:", flush=True)
    for k, v in rows:
        print(f"  {k:<18} {v}", flush=True)
