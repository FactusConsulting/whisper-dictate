"""Press-to-capture hotkey binding: the CLI orchestration (#258 part 1).

The headless ``--capture-hotkey`` flow: prompt the user to press & hold their
push-to-talk key(s), run the capture, surface the canonical chord back for
confirmation, then persist it into the ``key`` setting in config.json.

This is orchestration only. The chord ASSEMBLY is the pure, unit-tested
:class:`whisper_dictate.vp_keys_capture.ChordCapture`; the live keyboard hook is
:func:`whisper_dictate.vp_keys_capture_io.capture_chord_pynput`. Both the capture
step and the config writer are injectable, so the whole flow unit-tests headless
without a keyboard or real disk.

Kept in its own module (rather than piling onto the already-large vp_setup.py) to
respect the module-size rule. It reuses vp_setup's env-line formatters so the
"equivalent env var" hint matches the wizard's output exactly. Like vp_setup it
imports NO ML deps, and imports the live pynput listener LAZILY so merely
importing this module never installs a global keyboard hook.
"""
from __future__ import annotations

import sys
from pathlib import Path
from typing import Callable, TextIO

from whisper_dictate.vp_config import save_config
from whisper_dictate.vp_setup import format_bash_lines, format_powershell_lines

# The PTT chord is the ``key`` config setting (env VOICEPI_KEY); persisting the
# captured chord means writing this one key into config.json.
HOTKEY_SETTING = "key"


def persist_hotkey(
    chord: str,
    *,
    config_writer: Callable[[dict[str, str]], Path] | None = None,
) -> Path:
    """Write the captured ``chord`` into the PTT ``key`` setting in config.json.

    Merges over the EXISTING config (so capturing a hotkey never drops the user's
    other settings) and writes the whole map back. ``config_writer`` is injected
    by the tests; production uses :func:`whisper_dictate.vp_config.save_config`.
    """
    from whisper_dictate.vp_config import load_config

    writer = config_writer or save_config
    # Load the existing config preserving the original types (numbers, booleans,
    # null) — converting every value to str would corrupt siblings (null→"None",
    # true→"True", 42→"42") and break save_config's None/"" pruning logic.
    data = dict(load_config())
    data[HOTKEY_SETTING] = chord
    return writer(data)


def _resolve_input_fn(
    input_fn: Callable[[str], str] | None, stdin: TextIO | None
) -> Callable[[str], str]:
    """Pick a y/N prompt reader: the injected one, real input on a TTY, else
    scripted lines from a (piped/test) stdin so nothing blocks on no terminal."""
    if input_fn is not None:
        return input_fn
    in_stream = stdin or sys.stdin
    if bool(getattr(in_stream, "isatty", lambda: False)()):
        return input
    line_iter = iter(in_stream.readlines())

    def _scripted(_prompt: str) -> str:
        try:
            return next(line_iter).rstrip("\n")
        except StopIteration:
            return ""

    return _scripted


def run_capture_hotkey(
    *,
    capture_fn: Callable[..., str | None] | None = None,
    input_fn: Callable[[str], str] | None = None,
    output_fn: Callable[[str], None] | None = None,
    config_writer: Callable[[dict[str, str]], Path] | None = None,
    stdin: TextIO | None = None,
    stdout: TextIO | None = None,
    allow_media: bool = False,
    timeout: float | None = 30.0,
) -> int:
    """Press-to-capture flow: press & hold the hotkey, release, confirm, save.

    Steps: print "press your hotkey", run the capture, surface the canonical
    chord back for confirmation (``y`` to save, anything else to discard), then
    write it into the ``key`` setting. Returns a process exit code: 0 on save,
    1 if nothing was captured (e.g. timeout) or the user declined.

    ``capture_fn`` defaults to the live pynput listener (imported lazily);
    ``allow_media`` enables the experimental headset/media-key capture path
    (#258 part 2); ``timeout`` bounds the live wait so a headless run can't hang.
    """
    out = stdout or sys.stdout

    def _emit(text: str = "") -> None:
        print(text, file=out, flush=True)

    output = output_fn or _emit
    ask = _resolve_input_fn(input_fn, stdin)

    if capture_fn is None:
        from whisper_dictate.vp_keys_capture_io import capture_chord_pynput
        capture_fn = capture_chord_pynput

    output("Press and hold your push-to-talk key(s), then release.")
    output("What you press is what gets bound (left/right modifiers bind the same).")
    if allow_media:
        output("(experimental) media / headset buttons are also captured.")
    output("Listening…")

    chord = capture_fn(allow_media=allow_media, timeout=timeout)
    if not chord:
        output("No hotkey captured (timed out or nothing held). Nothing changed.")
        return 1

    output("")
    output(f"Captured hotkey: {chord}")
    answer = ask("Bind this as the push-to-talk hotkey? [y/N]: ").strip().lower()
    if answer not in ("y", "yes"):
        output("Discarded. Nothing changed.")
        return 1

    path = persist_hotkey(chord, config_writer=config_writer)
    output("")
    output(f"Saved hotkey '{chord}' to: {path}")
    output("# Equivalent env var (if you prefer env over config.json):")
    output(format_powershell_lines({HOTKEY_SETTING: chord}).rstrip("\n"))
    output(format_bash_lines({HOTKEY_SETTING: chord}).rstrip("\n"))
    return 0
