#!/usr/bin/env python3
"""Manual smoke test for text injection targets.

Focus the target input field, run this script, and leave focus alone until
the countdown finishes. It uses the same InjectMixin path as the app, without
recording audio or loading Whisper.
"""
from __future__ import annotations

import argparse
import time

from pynput import keyboard

from whisper_dictate.vp_inject import InjectMixin


DEFAULT_TEXT = "hello world - spaces stay intact, ae oe aa, 123."


class InjectionSmoke(InjectMixin):
    def __init__(self, mode: str):
        self.mode = mode
        self._kb = keyboard.Controller()
        self._inject_target_xwin = None
        self._inject_target_title = None
        self._inject_target_process = None
        self._xkb_layout = ""
        self._keycode_map = {}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("auto", "type", "paste", "print"),
                        default="auto")
    parser.add_argument("--text", default=DEFAULT_TEXT)
    parser.add_argument("--delay", type=float, default=3.0,
                        help="seconds before injection, so you can focus target")
    args = parser.parse_args()

    smoke = InjectionSmoke(args.mode)
    print(f"Focus target now. Injecting in {args.delay:g}s...", flush=True)
    time.sleep(max(0.0, args.delay))
    smoke._capture_target_window()
    smoke._inject(args.text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
