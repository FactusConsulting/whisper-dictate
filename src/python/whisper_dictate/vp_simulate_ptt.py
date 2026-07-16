"""Library-first PTT simulator: WAV in, transcript + injection-preview out.

This is the FIRST reference implementation of the library-first pattern for
whisper-dictate: every UI/hotkey-driven pipeline stage should also be callable
from a headless CLI so that a plain build container (e.g. ubuntu:26.04) can
verify the full app end to end without an OS-level keyboard hook or a real
microphone.

The simulator drives the SAME stages the live push-to-talk loop drives (see
:meth:`whisper_dictate.vp_dictate.Dictate._stop_and_transcribe`) — everything
AFTER audio capture:

  * decode the WAV to the same mono 16 kHz int16 PCM the live capture yields,
  * transcribe via ``_transcribe_detail`` (dictionary is applied inside),
  * post-process + format commands,
  * either DRY-RUN (default: print what would be typed) or INJECT (opt-in).

Kept intentionally small — no new global state, no side effects beyond stdout
and (with ``--inject``) the injection target. New product code added to the
live PTT pipeline should keep this simulator working; a pytest exercises the
whole chain against a fixture WAV to catch regressions.
"""
from __future__ import annotations

import argparse
import json
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path


@dataclass
class SimulatePttResult:
    """Structured outcome of one simulate-ptt run.

    Mirrors the same fields the live utterance event exposes for the pieces
    the simulator can produce, but stays small so downstream code (tests, the
    Rust CLI forward) can consume it directly. Serialise via
    :meth:`to_event` for the JSON output.
    """

    text: str
    raw_text: str
    dictionary_text: str
    post_text: str
    injected: bool
    inject_strategy: str | None
    inject_mode: str
    language: str
    audio_duration_s: float
    compute_s: float
    wav_path: str
    dictionary_replacements: list = field(default_factory=list)
    dictionary_terms: list = field(default_factory=list)

    def to_event(self) -> dict:
        payload = asdict(self)
        payload["event"] = "simulate_ptt"
        return payload


def simulate_ptt(
    model,
    wav_path: str | Path,
    *,
    lang: str | None = None,
    inject: bool = False,
) -> SimulatePttResult:
    """Run the full PTT-after-capture pipeline on ``wav_path``.

    ``model`` is a loaded whisper model as produced by
    :func:`whisper_dictate.vp_transcribe.load_stt_model`. Pass a stub model
    for tests: this function only calls ``_transcribe_detail`` which is easy
    to patch.

    Returns a :class:`SimulatePttResult`. Emits the same ``[gate]`` /
    ``[post]`` / ``[format]`` / ``[inject]`` progress lines as the live path
    so a simulate-ptt log reads like a real dictation.

    ``inject=False`` (the default) is the SAFE dry-run: nothing is typed —
    the caller just sees the ``[simulate-ptt] would inject: ...`` line. Set
    ``inject=True`` to actually invoke the injection backend. Only the
    direct-typing strategy is implemented in this POC; the ``paste`` mode
    from the live PTT loop needs a headless clipboard adapter that's kept
    as follow-up work.
    """
    from whisper_dictate.vp_audio_file import load_audio_file
    from whisper_dictate.vp_format import apply_format_commands
    from whisper_dictate.vp_postprocess import postprocess_text
    from whisper_dictate.vp_transcribe import _transcribe_detail

    wav = Path(wav_path)
    pcm = load_audio_file(wav)

    t0 = time.monotonic()
    tr = _transcribe_detail(model, pcm, lang)
    compute_s = time.monotonic() - t0

    raw_text = tr.raw_text or tr.text
    dictionary_text = tr.text
    post = postprocess_text(dictionary_text)
    fmt = apply_format_commands(post.text)
    final_text = fmt.text

    injected = False
    strategy: str | None = None
    if not final_text:
        print("[simulate-ptt] no text produced (empty transcription)", flush=True)
    elif inject:
        strategy = _run_injection(final_text)
        injected = True
    else:
        preview = final_text if len(final_text) < 200 else final_text[:197] + "..."
        print(f"[simulate-ptt] dry-run — would inject: {preview!r}", flush=True)

    return SimulatePttResult(
        text=final_text,
        raw_text=raw_text,
        dictionary_text=dictionary_text,
        post_text=post.text,
        injected=injected,
        inject_strategy=strategy,
        inject_mode="type" if inject else "dry-run",
        language=tr.language or lang or "auto",
        audio_duration_s=tr.duration_s,
        compute_s=compute_s,
        wav_path=str(wav),
        dictionary_replacements=list(tr.dictionary_replacements or []),
        dictionary_terms=list(tr.dictionary_terms or []),
    )


def _run_injection(text: str) -> str | None:
    """Actually inject ``text`` via pynput.

    Kept deliberately minimal for the POC: the live PTT injection mixin
    (:class:`whisper_dictate.vp_inject.InjectMixin`) carries a lot of state
    (target-window capture, ydotoold bootstrap, Wayland vs X11 branching,
    paste fallbacks) that only make sense when a real key press initiated
    the flow. Rather than fake all of that from a CLI context, ``--inject``
    from simulate-ptt calls ``pynput.keyboard.Controller().type(text)`` — the
    same primitive the live ``type`` strategy ultimately reaches. Paste mode
    is NOT wired here on purpose: it would need a headless clipboard adapter
    plus a synthetic Ctrl+V, and shipping it without either would silently
    fall back to typing while claiming to have pasted (the exact mismatch a
    reviewer caught in the first draft of this POC).
    """
    from pynput import keyboard

    controller = keyboard.Controller()
    controller.type(text)
    print(f"[simulate-ptt] injected {len(text)} chars via pynput (type)",
          flush=True)
    return "pynput-type"


def _print_result(result: SimulatePttResult, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(result.to_event(), ensure_ascii=False,
                         separators=(",", ":")),
              flush=True)
    else:
        print(result.text, flush=True)


# --------------------------------------------------------------------------
# Standalone entry point: ``python -m whisper_dictate.vp_simulate_ptt``.
# The main runtime module (``whisper_dictate.runtime``) wires the same flags
# into its argparse so ``whisper-dictate --simulate-ptt`` and the Rust
# ``whisper-dictate simulate-ptt`` subcommand end up in the same place.
# --------------------------------------------------------------------------


def _build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(
        prog="whisper-dictate simulate-ptt",
        description=(
            "Run the full PTT pipeline against a WAV file (no microphone, no "
            "keyboard hook). Default is a dry-run that prints what would be "
            "injected; pass --inject to type the transcript into the active "
            "window."
        ),
    )
    ap.add_argument("--wav", required=True, metavar="PATH",
                    help="Path to the input audio (16-bit PCM WAV native; "
                         "other formats need ffmpeg).")
    inject_group = ap.add_mutually_exclusive_group()
    inject_group.add_argument("--dry-run", action="store_true",
                              help="Print the would-inject text and exit "
                                   "(the default).")
    inject_group.add_argument("--inject", action="store_true",
                              help="Really call the injection backend and "
                                   "type into the active window. Off by "
                                   "default because it moves the user's "
                                   "cursor. Only the direct-typing strategy "
                                   "is implemented in this POC — a headless "
                                   "paste strategy is future work.")
    ap.add_argument("--lang", default=None,
                    help="Spoken language hint (da, en, ...); omit to let "
                         "Whisper auto-detect.")
    ap.add_argument("--model", default=None,
                    help="Whisper model name (default: read from the config "
                         "or VOICEPI_MODEL).")
    ap.add_argument("--device", default=None, choices=("auto", "cuda", "cpu"),
                    help="Compute device (default: auto).")
    ap.add_argument("--json", action="store_true",
                    help="Emit the result as a single JSON line instead of "
                         "the transcript text.")
    return ap


def _load_model_for_cli(args) -> tuple[object, str, str, str]:
    from whisper_dictate.vp_cli import MODEL_NAME, _resolve_device
    from whisper_dictate.vp_transcribe import load_stt_model

    model_name = args.model or MODEL_NAME
    device, compute_type = _resolve_device(args.device or "auto")
    model = load_stt_model(model_name, device, compute_type)
    return model, model_name, device, compute_type


def main(argv: list[str] | None = None) -> int:
    ap = _build_parser()
    args = ap.parse_args(argv)

    model, _model_name, _device, _compute_type = _load_model_for_cli(args)
    result = simulate_ptt(
        model,
        args.wav,
        lang=args.lang,
        inject=bool(args.inject),
    )
    _print_result(result, as_json=bool(args.json))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
