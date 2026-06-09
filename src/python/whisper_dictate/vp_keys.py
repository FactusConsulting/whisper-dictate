"""Global push-to-talk hotkey backends (evdev + pynput).

Extracted from runtime.py as a mixin, mirroring the InjectMixin split. The
``Dictate`` loop mixes this in to get key detection:

  * evdev — reads /dev/input/event* directly; required on pure Wayland where
    pynput's Xorg backend misses Wayland-native windows. Needs the 'input' group.
  * pynput — X11 / Windows / macOS fallback, with the multi-press quit chord.

Both backends drive ``self._start()`` / ``self._stop_and_transcribe()`` on the
combined Dictate instance; evdev / pynput are imported lazily inside the methods.
"""
from __future__ import annotations

import os
import sys
import threading
import time

from whisper_dictate.vp_cli import QUIT_COUNT, QUIT_KEY, QUIT_WINDOW_MS


class _PynputListener:
    """Push-to-talk state machine for the pynput backend.

    Holds the mutable press / recording / quit-streak state so the listener
    callbacks stay tiny (the previous nested closures pushed _run_pynput's
    cognitive complexity well past the limit). ``owner`` is the Dictate
    instance providing ``_start`` / ``_stop_and_transcribe``.
    """

    def __init__(self, owner, targets: set, quit_key) -> None:
        self._owner = owner
        self._targets = targets
        self._quit_key = quit_key
        self._pressed: set = set()
        self._recording = False
        self._quit_count = 0
        self._quit_last = 0.0

    def _quit_chord(self, k) -> bool:
        # Track the consecutive quit-key streak; True once it reaches
        # QUIT_COUNT within QUIT_WINDOW_MS. Any non-quit key resets the streak.
        if k != self._quit_key:
            self._quit_count = 0
            return False
        if QUIT_COUNT <= 0:
            return False
        now = time.monotonic()
        if now - self._quit_last <= QUIT_WINDOW_MS / 1000.0:
            self._quit_count += 1
        else:
            self._quit_count = 1
        self._quit_last = now
        return self._quit_count >= QUIT_COUNT

    def on_press(self, k):
        # Returning False stops the pynput listener (quit chord fired).
        if self._quit_chord(k):
            return False
        if k == self._quit_key:
            return None  # quit key never joins the PTT-key set
        self._pressed.add(k)
        if self._targets.issubset(self._pressed) and not self._recording:
            self._recording = True
            self._owner._start()
        return None

    def on_release(self, k):
        if k in self._targets:
            self._pressed.discard(k)
            if self._recording and not self._targets.issubset(self._pressed):
                self._recording = False
                threading.Thread(target=self._owner._stop_and_transcribe,
                                 daemon=True).start()
        return None


class KeyBackendMixin:
    # pynput key name → evdev key code mapping for common PTT keys
    _EVDEV_MAP = {
        'ctrl_l': 'KEY_LEFTCTRL',   'ctrl_r': 'KEY_RIGHTCTRL',
        'shift_l': 'KEY_LEFTSHIFT', 'shift_r': 'KEY_RIGHTSHIFT',
        'alt_l': 'KEY_LEFTALT',     'alt_r': 'KEY_RIGHTALT',
        'super_l': 'KEY_LEFTMETA',  'super_r': 'KEY_RIGHTMETA',
        **{f'f{i}': f'KEY_F{i}' for i in range(1, 13)},
    }

    def _evdev_target_codes(self, evdev, key_names: list[str]) -> set[int]:
        # Resolve pynput-style key names to evdev keycodes, exiting on anything
        # unmapped (the supported set is _EVDEV_MAP).
        target_codes: set[int] = set()
        for kn in key_names:
            ecname = self._EVDEV_MAP.get(kn)
            if ecname is None:
                sys.exit(f"unknown key '{kn}' for evdev "
                         f"(supported: {', '.join(self._EVDEV_MAP)})")
            code = getattr(evdev.ecodes, ecname, None)
            if code is None:
                sys.exit(f"evdev has no keycode '{ecname}'")
            target_codes.add(code)
        return target_codes

    def _evdev_open_keyboards(self, evdev) -> list:
        # Open all input devices that have EV_KEY capability (keyboards).
        devices = []
        for path in evdev.list_devices():
            try:
                d = evdev.InputDevice(path)
                if evdev.ecodes.EV_KEY in d.capabilities():
                    devices.append(d)
            except Exception:
                pass
        return devices

    @staticmethod
    def _evdev_close(devices) -> None:
        for d in devices:
            try:
                d.close()
            except Exception:
                pass

    def _evdev_apply_event(self, ev, evdev, target_codes: set[int],
                           pressed: set[int], recording: bool) -> bool:
        # PTT state machine for a single evdev event. Mutates ``pressed`` and
        # returns the (possibly updated) recording flag. Pure given a fake evdev,
        # which is what the unit tests exercise.
        if ev.type != evdev.ecodes.EV_KEY or ev.code not in target_codes:
            return recording
        if ev.value == evdev.KeyEvent.key_down:
            pressed.add(ev.code)
            if target_codes.issubset(pressed) and not recording:
                self._start()
                return True
        elif ev.value == evdev.KeyEvent.key_up:
            pressed.discard(ev.code)
            if recording and not target_codes.issubset(pressed):
                threading.Thread(target=self._stop_and_transcribe,
                                 daemon=True).start()
                return False
        return recording

    def _run_evdev(self, key_names: list[str]):
        # Global hotkey detection via evdev — reads /dev/input/event* directly.
        # Works on pure Wayland where pynput's Xorg backend misses events from
        # Wayland-native windows. Requires user to be in the 'input' group.
        import evdev
        import select

        target_codes = self._evdev_target_codes(evdev, key_names)
        devices = self._evdev_open_keyboards(evdev)
        if not devices:
            sys.exit("evdev: no keyboard devices found — are you in the 'input' group?")

        pressed: set[int] = set()
        recording = False

        print(f"whisper-dictate [lang={self.lang or 'auto'}] (evdev). Hold "
              f"[{self.key}] to talk. Ctrl+C to quit.", flush=True)

        try:
            while True:
                r, _, _ = select.select(devices, [], [], 0.5)
                for dev in r:
                    try:
                        events = dev.read()
                    except OSError:
                        continue
                    for ev in events:
                        recording = self._evdev_apply_event(
                            ev, evdev, target_codes, pressed, recording)
        except KeyboardInterrupt:
            pass
        finally:
            self._evdev_close(devices)
        print("\nbye", flush=True)

    def _have_evdev(self) -> bool:
        try:
            import evdev  # noqa: F401
            return True
        except ImportError:
            return False

    def _pynput_targets(self, keyboard, key_names: list[str]) -> set:
        targets = set()
        for kn in key_names:
            k = getattr(keyboard.Key, kn, None)
            if k is None:
                sys.exit(f"unknown key '{kn}' (e.g. ctrl_r, shift_r, alt_r, f9)")
            targets.add(k)
        return targets

    def _pynput_quit_key(self, keyboard):
        quit_key = getattr(keyboard.Key, QUIT_KEY, None)
        if quit_key is None and len(QUIT_KEY) == 1:
            quit_key = QUIT_KEY
        if quit_key is None:
            sys.exit(f"unknown quit key '{QUIT_KEY}' (e.g. esc, f12, q)")
        return quit_key

    def _run_pynput(self, key_names: list[str]) -> None:
        # X11 / Windows / macOS fallback.
        from pynput import keyboard

        targets = self._pynput_targets(keyboard, key_names)
        quit_key = self._pynput_quit_key(keyboard)

        quit_hint = f"{QUIT_COUNT}× {QUIT_KEY} or Ctrl+C" if QUIT_COUNT > 0 else "Ctrl+C"
        print(f"whisper-dictate [lang={self.lang or 'auto'}] (pynput). Hold "
              f"[{self.key}] to talk. {quit_hint} to quit.", flush=True)

        state = _PynputListener(self, targets, quit_key)
        ln = keyboard.Listener(on_press=state.on_press, on_release=state.on_release)
        ln.start()
        try:
            ln.join()
        except KeyboardInterrupt:
            pass
        finally:
            ln.stop()
        print("\nbye", flush=True)

    def run(self):
        # Support chord keys: 'shift_r+ctrl_r' means hold both simultaneously.
        # On Wayland: use evdev (reads /dev/input/event* — global, layout-agnostic).
        # On X11: fall back to pynput's xorg backend.
        key_names = [n.strip() for n in self.key.split('+')]
        on_wayland = bool(os.environ.get('WAYLAND_DISPLAY'))

        if on_wayland and self._have_evdev():
            self._run_evdev(key_names)
            return
        if on_wayland:
            sys.exit("Wayland requires evdev for global hotkeys. "
                     "Run whisper-dictate install again or install requirements/cpu.txt; "
                     "use --doctor for a full health check.")

        self._run_pynput(key_names)
