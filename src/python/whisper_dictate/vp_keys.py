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
from whisper_dictate.vp_config import get_value
from whisper_dictate.vp_keys_solo import (
    SoloModifierGuard,
    all_targets_have_distinct_match as _all_targets_have_distinct_match,
    held_keys_cleared_by_release as _held_keys_cleared_by_release,
    is_bare_modifier_binding,
    key_name as _key_name,
    modifier_family as _modifier_family,
    modifier_matches as _modifier_matches,
)


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() not in ("", "0", "false", "no", "off")


def _key_label(k) -> str:
    """Best-effort short label for a held key (pynput Key/str or evdev code).

    Used only for the human-readable "chord detected (ctrl+c)" log line, so a
    rough name is fine. pynput ``Key.ctrl_l`` → ``ctrl_l``; a pynput KeyCode
    with a ``.char`` → that char; an evdev int code → ``key#<code>``.
    """
    name = getattr(k, "name", None)  # pynput Key enum
    if name:
        return str(name)
    char = getattr(k, "char", None)  # pynput KeyCode
    if char:
        return str(char)
    if isinstance(k, int):
        return f"key#{k}"
    return str(k)


def _chord_desc(targets, foreign) -> str:
    """``ctrl+c``-style description of the modifier(s) plus the foreign key."""
    mods = "+".join(sorted(_key_label(t) for t in targets))
    return f"{mods}+{_key_label(foreign)}"


def _toggle_mode_enabled() -> bool:
    """Whether toggle dictation mode is enabled (VOICEPI_TOGGLE / toggle_mode).

    Read once when a listener is constructed — the listeners capture the chosen
    behaviour at startup, so this is a restart-only setting (see RESTART_KEYS).
    """
    return _truthy(get_value("VOICEPI_TOGGLE"))


class _PynputListener:
    """Push-to-talk / toggle state machine for the pynput backend.

    Holds the mutable press / recording / quit-streak state so the listener
    callbacks stay tiny (the previous nested closures pushed _run_pynput's
    cognitive complexity well past the limit). ``owner`` is the Dictate
    instance providing ``_start`` / ``_stop_and_transcribe``.

    ``toggle_mode`` selects the behaviour, captured once at construction:
      * hold-to-talk (default): start on chord press, stop on chord release.
      * toggle: the chord PRESS starts recording; pressing it again stops and
        transcribes. The release event is ignored for recording purposes (it
        still updates the held set so chord tracking stays correct).

    Both modes act on the *rising edge* only: a held key repeats press events
    on most platforms, so we latch when the chord is complete and re-arm only
    once it is no longer fully held — repeats never re-trigger.

    **Side-specific matching (reverses #254).** Every key comparison — chord
    completion, the solo guard's target/foreign test, and the quit key — routes
    through :func:`modifier_matches`: a ``ctrl_l`` binding matches left Ctrl (and
    the generic ``Key.ctrl`` fallback) but NOT right Ctrl. The chord is tracked
    over the SET of raw held tokens (``_held_keys``) and is complete when every
    target NAME has a held key matching it, so a member arriving as the generic
    family variant still satisfies its side-specific target.
    """

    def __init__(self, owner, targets: set, quit_key, toggle_mode: bool = False,
                 solo_guard: SoloModifierGuard | None = None) -> None:
        self._owner = owner
        # Target / quit SETTING NAMES for side-aware matching: a pynput Key → its
        # .name ("ctrl_l"), a plain string token → itself. modifier_matches()
        # handles both modifier (side-aware + generic fallback) and non-modifier
        # (esc / f12 / single char → plain equality) names.
        self._target_names = [self._name_of(t) for t in targets]
        self._quit_name = self._name_of(quit_key)
        self._toggle_mode = toggle_mode
        # Raw held tokens matching at least one target name (the chord members
        # down); chord completion is computed over this set.
        self._held_keys: set = set()
        self._recording = False
        # True while the chord is fully held; reset when it breaks. Guards
        # against key-repeat re-firing the toggle while the key stays down.
        self._chord_latched = False
        self._quit_count = 0
        self._quit_last = 0.0
        # Bare-modifier "press alone" guard (no-op unless the binding is all bare
        # modifiers — see vp_keys_solo). We install a SIDE-AWARE target predicate
        # so a chord member arriving as the generic family variant counts as a
        # target (fail-safe) while the OPPOSITE side counts as foreign (#254
        # reversal).
        if solo_guard is None:
            self._solo = SoloModifierGuard(set(), enabled=False)
        else:
            self._solo = solo_guard
            self._solo.set_is_target(self._key_is_target)

    @staticmethod
    def _name_of(token):
        name = _key_name(token)
        return name if name is not None else token

    def _key_is_target(self, k) -> bool:
        # Side-aware: does held key ``k`` satisfy any bound target name?
        return any(_modifier_matches(k, name) for name in self._target_names)

    def _chord_complete(self) -> bool:
        # Complete when every target name can be paired with a DISTINCT held key
        # (a 1:1 assignment), not merely "some held key matches each name". The
        # generic fallback makes one held Key.ctrl match BOTH ctrl_l and ctrl_r,
        # so the naive form would complete a ctrl_l+ctrl_r both-sides binding on a
        # single physical Ctrl — this requires two held keys for a two-key chord.
        return _all_targets_have_distinct_match(self._target_names, self._held_keys)

    def _matches_quit(self, k) -> bool:
        return _modifier_matches(k, self._quit_name)

    def _quit_chord(self, k) -> bool:
        # Consecutive quit-key streak; True once it reaches QUIT_COUNT within
        # QUIT_WINDOW_MS. Any non-quit key resets it. The quit match is side-aware
        # (own side or generic fallback, not the opposite side).
        if not self._matches_quit(k):
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
        # Bare-modifier rule 2: a foreign key down while recording forms a chord
        # (Ctrl+C, or the WRONG-SIDE modifier) — cancel and discard, even if it
        # is the quit key. should_cancel_on_press uses the side-aware predicate.
        if self._recording and self._solo.should_cancel_on_press(k):
            self._cancel_chord(k)
            return None
        if self._matches_quit(k):
            # Treat the quit key as a foreign held key for rule-1 purposes so
            # holding it then pressing a bare-modifier PTT key does not start
            # dictation (the guard would never have seen it as held otherwise).
            self._solo.note_press(k)
            return None  # quit key never joins the PTT-key set
        self._solo.note_press(k)
        if self._key_is_target(k):
            self._held_keys.add(k)
        chord_complete = self._chord_complete()
        # Rising edge only: a held key repeats press events, so act exactly once
        # when the chord first becomes complete and re-arm only after it breaks.
        rising_edge = chord_complete and not self._chord_latched
        if chord_complete:
            self._chord_latched = True
        if rising_edge:
            # Bare-modifier rule 1: do not start if any other key is already
            # held (the user is forming a shortcut, not dictating).
            if not self._solo.may_start_on_target_down():
                return None
            if self._toggle_mode:
                self._toggle_recording()
            elif not self._recording:
                self._recording = True
                self._owner._start()
        return None

    def _cancel_chord(self, k) -> None:
        # A foreign key joined the held PTT modifier — discard the in-flight
        # dictation (no transcription / injection). _chord_latched stays set so
        # the held modifier does not re-trigger via key-repeat; it re-arms on
        # release like any other chord break.
        self._recording = False
        print(f"[keys] chord detected ({_chord_desc(self._held_keys, k)}) "
              f"— dictation cancelled", flush=True)
        # Capture the recording generation NOW so a delayed cancel cannot discard
        # a later recording (release + re-press before this thread runs).
        epoch = getattr(self._owner, "_record_epoch", None)
        threading.Thread(target=self._owner._cancel_and_discard,
                         args=(epoch,), daemon=True).start()

    def on_release(self, k):
        self._solo.note_release(k)
        if self._key_is_target(k):
            self._discard_held(k)
            if not self._chord_complete():
                # Chord broken: re-arm the rising-edge latch. Hold-to-talk stops
                # on release; toggle mode ignores release (acts only on press).
                self._chord_latched = False
                if not self._toggle_mode and self._recording:
                    self._recording = False
                    threading.Thread(target=self._owner._stop_and_transcribe,
                                     daemon=True).start()
        return None

    def _discard_held(self, k) -> None:
        # Side-aware release clearing, shared verbatim with the solo guard via
        # ``held_keys_cleared_by_release`` so both release paths agree. The SAME
        # physical key may be reported under different variants on press vs
        # release (ctrl_l down, generic ctrl up): a GENERIC release clears the
        # whole family (side unknown → fail-safe so the chord can break), but a
        # SIDE-SPECIFIC release drops only the same side (alt_gr≡alt_r) plus any
        # held generic — LEAVING the opposite side held, so releasing left Ctrl
        # of a both-sides chord doesn't wrongly drop a still-held right Ctrl.
        # Non-modifier keys (no family) use plain token equality.
        for hk in _held_keys_cleared_by_release(self._held_keys, k):
            self._held_keys.discard(hk)

    def _toggle_recording(self) -> None:
        # Toggle-mode press: start if idle, otherwise stop and transcribe. The
        # stop runs on a thread exactly like the hold-mode release handler so the
        # transcription pass never blocks the keyboard listener.
        if self._recording:
            self._recording = False
            threading.Thread(target=self._owner._stop_and_transcribe,
                             daemon=True).start()
        else:
            self._recording = True
            self._owner._start()


class KeyBackendMixin:
    # pynput key name → evdev key code mapping for common PTT keys
    _EVDEV_MAP = {
        'ctrl_l': 'KEY_LEFTCTRL',   'ctrl_r': 'KEY_RIGHTCTRL',
        'shift_l': 'KEY_LEFTSHIFT', 'shift_r': 'KEY_RIGHTSHIFT',
        'alt_l': 'KEY_LEFTALT',     'alt_r': 'KEY_RIGHTALT',
        # AltGr is physically the right Alt on most layouts; capture records it as
        # "alt_gr" (its pynput name), so map it here too or the evdev backend
        # would sys.exit("unknown key 'alt_gr'") on a captured AltGr binding.
        'alt_gr': 'KEY_RIGHTALT',
        'super_l': 'KEY_LEFTMETA',  'super_r': 'KEY_RIGHTMETA',
        # pynput names the Win/Cmd modifier family "cmd" (cmd_l / cmd_r / cmd);
        # evdev uses KEY_LEFTMETA / KEY_RIGHTMETA (exposed above as super_l/r).
        # Add cmd_l / cmd_r aliases so a hotkey captured with the pynput backend
        # (which emits "cmd_r") can also be used with the evdev backend without
        # a sys.exit("unknown key 'cmd_r' for evdev").
        'cmd_l': 'KEY_LEFTMETA',    'cmd_r': 'KEY_RIGHTMETA',
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
                           pressed: set[int], recording: bool, *,
                           toggle_mode: bool = False,
                           latched: list[bool] | None = None,
                           solo: SoloModifierGuard | None = None) -> bool:
        # PTT / toggle state machine for a single evdev event. Mutates ``pressed``
        # (and the ``latched`` rising-edge guard, when provided) and returns the
        # (possibly updated) recording flag. Pure given a fake evdev, which is
        # what the unit tests exercise.
        #
        # evdev key values: 1 = down, 2 = autorepeat (held), 0 = up. We act only
        # on the down (value 1) transition and ignore repeats (value 2 is neither
        # key_down nor key_up here), so a held key never re-triggers.
        #
        # ``solo`` is the bare-modifier guard. When enabled it tracks ALL key
        # codes (including non-target/foreign keys) so it can (1) refuse to start
        # inside a chord and (2) cancel an in-flight dictation when a foreign key
        # joins the held modifier. When None/disabled, foreign keys are ignored
        # exactly as before.
        if ev.type != evdev.ecodes.EV_KEY:
            return recording
        is_target = ev.code in target_codes
        if not is_target and (solo is None or not solo.enabled):
            return recording
        if ev.value == evdev.KeyEvent.key_down:
            return self._evdev_key_down(ev, target_codes, pressed, recording,
                                        is_target, toggle_mode, latched, solo)
        if ev.value == evdev.KeyEvent.key_up:
            return self._evdev_key_up(ev, target_codes, pressed, recording,
                                      is_target, toggle_mode, latched, solo)
        # value == 2: OS autorepeat. Refresh the guard timestamp for a tracked
        # foreign key so a genuinely-held key does not expire out of the held set
        # (phantom-key self-heal). Target-key autorepeat behaviour is unchanged —
        # a held TARGET self-prunes after the expiry, which is intentionally
        # harmless: foreign_key_held() only counts keys OUTSIDE the target set,
        # so a pruned target can never block a start or trigger a cancel.
        # (pynput differs — target repeats refresh via note_press — and that
        # asymmetry is fine for the same reason.)
        if (solo is not None and ev.value == evdev.KeyEvent.key_hold
                and not is_target):
            solo.note_repeat(ev.code)
        return recording

    def _evdev_key_down(self, ev, target_codes, pressed, recording, is_target,
                        toggle_mode, latched, solo) -> bool:
        # Track every key in the guard first (newly-held vs key-repeat), so both
        # rules see the full held-key set.
        newly_held = solo.note_press(ev.code) if solo is not None else True
        # Bare-modifier rule 2: a NEW foreign key going down mid-recording forms
        # a chord (e.g. Ctrl held, then C) — cancel and discard.
        if (solo is not None and recording and newly_held
                and ev.code not in target_codes):
            print(f"[keys] chord detected ({_chord_desc(target_codes, ev.code)}) "
                  f"— dictation cancelled", flush=True)
            # Capture the recording generation NOW so a delayed cancel cannot
            # discard a later recording (release + re-press before this runs).
            epoch = getattr(self, "_record_epoch", None)
            threading.Thread(target=self._cancel_and_discard,
                             args=(epoch,), daemon=True).start()
            return False
        if not is_target:
            return recording  # foreign keydown: tracked above, nothing else to do
        pressed.add(ev.code)
        chord_complete = target_codes.issubset(pressed)
        # Rising edge: chord just became complete. ``latched`` carries the
        # arm/disarm state across calls (toggle mode); without it we fall
        # back to the legacy ``not recording`` guard (hold-mode callers).
        already_latched = latched is not None and latched[0]
        if chord_complete and latched is not None:
            latched[0] = True
        rising_edge = chord_complete and not already_latched
        # Bare-modifier rule 1: refuse to start if another key is already held.
        if rising_edge and solo is not None and not solo.may_start_on_target_down():
            return recording
        if toggle_mode:
            if rising_edge:
                return self._evdev_toggle(recording)
        elif chord_complete and not recording:
            self._start()
            return True
        return recording

    def _evdev_key_up(self, ev, target_codes, pressed, recording, is_target,
                      toggle_mode, latched, solo) -> bool:
        if solo is not None:
            solo.note_release(ev.code)
        if not is_target:
            return recording
        pressed.discard(ev.code)
        if not target_codes.issubset(pressed):
            if latched is not None:
                latched[0] = False  # re-arm the rising-edge guard
            if not toggle_mode and recording:
                threading.Thread(target=self._stop_and_transcribe,
                                 daemon=True).start()
                return False
        return recording

    def _evdev_toggle(self, recording: bool) -> bool:
        # Toggle-mode chord press: start if idle, else stop+transcribe on a
        # thread (mirrors the hold-mode release dispatch). Returns the new flag.
        if recording:
            threading.Thread(target=self._stop_and_transcribe,
                             daemon=True).start()
            return False
        self._start()
        return True

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
        toggle_mode = _toggle_mode_enabled()
        latched = [False]  # rising-edge guard shared across events
        solo = SoloModifierGuard(
            target_codes, enabled=is_bare_modifier_binding(key_names))

        verb = "Press" if toggle_mode else "Hold"
        suffix = (" Press again to stop." if toggle_mode else " to talk.")
        print(f"whisper-dictate [lang={self.lang or 'auto'}] (evdev). {verb} "
              f"[{self.key}]{suffix} Ctrl+C to quit.", flush=True)

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
                            ev, evdev, target_codes, pressed, recording,
                            toggle_mode=toggle_mode, latched=latched, solo=solo)
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
        toggle_mode = _toggle_mode_enabled()

        quit_hint = f"{QUIT_COUNT}× {QUIT_KEY} or Ctrl+C" if QUIT_COUNT > 0 else "Ctrl+C"
        verb = "Press" if toggle_mode else "Hold"
        suffix = (" Press again to stop." if toggle_mode else " to talk.")
        print(f"whisper-dictate [lang={self.lang or 'auto'}] (pynput). {verb} "
              f"[{self.key}]{suffix} {quit_hint} to quit.", flush=True)

        # Guard over the raw targets; _PynputListener installs a SIDE-AWARE target
        # predicate (modifier_matches), so a member arriving as the generic family
        # variant is a target while the OPPOSITE side is foreign (#254 reversal).
        solo = SoloModifierGuard(
            targets, enabled=is_bare_modifier_binding(key_names))
        state = _PynputListener(self, targets, quit_key, toggle_mode=toggle_mode,
                                solo_guard=solo)
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
