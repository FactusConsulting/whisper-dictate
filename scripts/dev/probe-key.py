#!/usr/bin/env python3
"""Probe a push-to-talk key/chord to verify pynput sees it on THIS machine.

Useful before committing to a VOICEPI_KEY value - if this probe doesn't
see the key, whisper-dictate won't either, and you'll just see a silent
non-trigger. Common gotchas this catches:

  * Pause/Break key missing on tenkeyless / laptop keyboards
  * Pause/Break intercepted by gaming-keyboard firmware (Razer/Corsair)
  * Caps_lock state-toggle behaviour (press fires, release doesn't -
    breaks hold-to-talk)
  * Multimedia keys eaten by OEM software before reaching pynput
  * Chords like ctrl_r+space being filtered by IMEs / IntelliSense

Hard-fail gotchas (the probe pre-warns BEFORE you press anything):

  * `pause`/`break` on Windows: pynput sees the keypress, but the
    Windows console interprets Pause as CTRL_BREAK_EVENT and aborts
    the foreground process (MKL Fortran runtime: `forrtl: error 200`).
    Recording starts, then the Python runtime crashes mid-utterance. Use `f9`.
  * `caps_lock`/`num_lock`/`scroll_lock` on Windows: stateful toggle
    keys. Only the press event fires reliably; release-on-hold is
    unreliable. Breaks the hold-to-talk model. Use a non-toggle key.

Usage:
  python scripts/dev/probe-key.py                    # passive: log EVERY key event
  python scripts/dev/probe-key.py pause              # active: confirm Pause arrives
  python scripts/dev/probe-key.py ctrl_r+space       # active: confirm a chord
  python scripts/dev/probe-key.py shift_r+ctrl_r 30  # custom duration (seconds)

Exit codes:
  0 - chord verified (or passive mode finished)
  1 - no events captured at all (OS not delivering)
  2 - events captured but full chord was never held together
  3 - invalid key name passed
"""
from __future__ import annotations

import sys
import time

try:
    from pynput import keyboard
except ImportError:
    sys.exit("pynput not installed. Install it: "
             "pip install pynput   (or just use the whisper-dictate venv)")


def _parse(spec: str) -> list[str]:
    return [k.strip() for k in spec.split("+") if k.strip()]


def _resolve(name: str):
    k = getattr(keyboard.Key, name, None)
    if k is None:
        print(f"unknown key '{name}'", file=sys.stderr)
        print("valid pynput Key names include:", file=sys.stderr)
        print("  modifiers: ctrl_l, ctrl_r, alt_l, alt_r, alt_gr, "
              "shift_l, shift_r, cmd_l, cmd_r", file=sys.stderr)
        print("  function:  f1..f20", file=sys.stderr)
        print("  special:   space, esc, enter, tab, backspace, delete, "
              "insert, home, end, page_up, page_down", file=sys.stderr)
        print("  toggles:   caps_lock, num_lock, scroll_lock, pause, "
              "print_screen", file=sys.stderr)
        print("  arrows:    up, down, left, right", file=sys.stderr)
        print("  media:     media_play_pause, media_next, media_previous, "
              "media_volume_up/down/mute", file=sys.stderr)
        sys.exit(3)
    return k


def _fmt(k) -> str:
    if hasattr(k, "name"):
        return f"Key.{k.name}"
    char = getattr(k, "char", None)
    if char is not None:
        return f"KeyCode({char!r})"
    vk = getattr(k, "vk", None)
    if vk is not None:
        return f"KeyCode(vk={vk})"
    return repr(k)


def _parse_args(argv: list[str]) -> tuple[str, float] | None:
    duration = 15.0
    chord_spec = argv[1] if len(argv) >= 2 else ""
    if len(argv) >= 2:
        chord_spec = argv[1]
    try:
        if len(argv) >= 3:
            duration = float(argv[2])
    except ValueError:
        print(f"invalid duration '{argv[2]}' (expected seconds)", file=sys.stderr)
        return None
    return chord_spec, duration


def _hard_fail_reasons() -> dict[str, str]:
    return {
        "pause": ("Windows console interprets Pause as CTRL_BREAK_EVENT "
                  "and aborts the foreground process. pynput sees the "
                  "press, but the Python runtime crashes mid-utterance with "
                  "'forrtl: error 200'. Use f9 instead."),
        "caps_lock": ("Stateful toggle - on Windows the press fires once "
                      "but the release-on-hold is unreliable. Breaks the "
                      "hold-to-talk model. Use f9 / shift_r+ctrl_r."),
        "num_lock": "Stateful toggle - same problem as caps_lock.",
        "scroll_lock": "Stateful toggle - same problem as caps_lock.",
    }


def _warn_known_bad_keys(names: list[str]) -> None:
    hard_fail = _hard_fail_reasons()
    for name in names:
        if name not in hard_fail:
            continue
        print(f"!! WARNING: '{name}' is a known-bad VOICEPI_KEY:")
        print(f"   {hard_fail[name]}")
        print("   This probe may still report 'OK', but the real app will fail.\n")


def _targets_for_chord(chord_spec: str):
    if not chord_spec:
        return None
    names = _parse(chord_spec)
    _warn_known_bad_keys(names)
    return {_resolve(n) for n in names}


def _print_probe_intro(chord_spec: str, targets, duration: float) -> None:
    if targets is not None:
        print(f"probing chord [{chord_spec}] for {duration:.0f}s - "
              f"hold all {len(targets)} key(s) together")
    else:
        print(f"passive probe for {duration:.0f}s - press any key, "
              f"every event is logged")
    print("(Ctrl+C to exit early)\n", flush=True)


def _probe_verdict(chord_spec: str, targets, events: int, chord_was_complete: bool) -> int:
    print(f"\n--- summary: {events} key event(s) captured ---")
    if targets is None:
        return 0  # passive mode, no verdict to render

    if events == 0:
        print(f"X  NO events from chord [{chord_spec}] - "
              "your OS isn't delivering these keys to pynput.")
        print("   Try a function key (e.g. f9) which is delivered reliably:")
        print("     setx VOICEPI_KEY f9")
        return 1

    if not chord_was_complete:
        print(f"!  events arrived but the full chord [{chord_spec}] was "
              "never held simultaneously.")
        print("   Either some keys weren't pressed together, or one of "
              "them isn't reaching pynput (e.g. Fn-layer keys).")
        return 2

    print(f"OK chord [{chord_spec}] works - safe to use:")
    print(f"   setx VOICEPI_KEY {chord_spec}")
    return 0


def main(argv: list[str]) -> int:
    parsed = _parse_args(argv)
    if parsed is None:
        return 3
    chord_spec, duration = parsed
    targets = _targets_for_chord(chord_spec)
    _print_probe_intro(chord_spec, targets, duration)

    pressed: set = set()
    chord_was_complete = False
    events = 0

    def on_press(k):
        nonlocal chord_was_complete, events
        events += 1
        pressed.add(k)
        marker = ""
        if targets is not None and targets.issubset(pressed) \
                and not chord_was_complete:
            marker = "  <-- CHORD COMPLETE"
            chord_was_complete = True
        print(f"  press   {_fmt(k)}{marker}", flush=True)

    def on_release(k):
        nonlocal chord_was_complete, events
        events += 1
        pressed.discard(k)
        marker = ""
        if targets is not None and chord_was_complete \
                and not targets.issubset(pressed):
            marker = "  <-- chord released"
            chord_was_complete = False
        print(f"  release {_fmt(k)}{marker}", flush=True)

    ln = keyboard.Listener(on_press=on_press, on_release=on_release)
    ln.start()
    try:
        t0 = time.monotonic()
        while time.monotonic() - t0 < duration:
            time.sleep(0.1)
    except KeyboardInterrupt:
        print("\n(ctrl-c)")
    finally:
        ln.stop()

    return _probe_verdict(chord_spec, targets, events, chord_was_complete)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
