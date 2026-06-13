"""Press-to-capture hotkey binding: the live pynput listener IO shell.

The thin IO half of issue #258 part 1. All the chord-assembly behaviour lives in
the pure, unit-tested :mod:`whisper_dictate.vp_keys_capture`; this module only
wires a real ``pynput.keyboard.Listener`` to a :class:`ChordCapture` and blocks
until the user has pressed-and-released a chord.

Kept deliberately tiny and free of decision logic so the untested surface (the
live global keyboard hook) is as small as possible.
"""
from __future__ import annotations

from whisper_dictate.vp_keys_capture import ChordCapture


def capture_chord_pynput(*, allow_media: bool = False, timeout: float | None = None) -> str | None:
    """Listen for a press-and-release chord and return its canonical name.

    Starts a pynput global keyboard listener, forwards every press/release into a
    :class:`ChordCapture`, and stops as soon as the capture emits a chord (the
    user let go of everything) — returning that chord string (``"ctrl_r"``,
    ``"shift_r+ctrl_r"`` …). Returns ``None`` if the listener stops without a
    capture (e.g. ``timeout`` elapsed).

    ``allow_media`` forwards to :class:`ChordCapture` (experimental headset/media
    capture, #258 part 2). ``timeout`` (seconds) bounds the wait so a headless
    invocation can never hang forever; ``None`` waits indefinitely.

    The listener callbacks return ``False`` to stop the listener once a chord is
    captured; pynput then unwinds ``Listener.join``.
    """
    from pynput import keyboard

    capture = ChordCapture(allow_media=allow_media)

    def on_press(key):
        capture.press(key)
        # Keep listening; the chord is only finalised on release.
        return None

    def on_release(key):
        chord = capture.release(key)
        if chord is not None:
            return False  # stop the listener: full chord captured
        return None

    listener = keyboard.Listener(on_press=on_press, on_release=on_release)
    listener.start()
    try:
        listener.join(timeout)
    finally:
        listener.stop()
    return capture.result
