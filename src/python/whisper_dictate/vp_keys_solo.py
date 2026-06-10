"""Bare-modifier "press alone" semantics for the push-to-talk key.

When the configured PTT key is a single *bare modifier* (ctrl / shift / alt /
win in any left/right variant), users expect it to dictate ONLY when pressed by
itself — not as part of an OS/app shortcut like Shift+Ctrl+X. This module holds
the small, backend-agnostic state machine that enforces that, shared by both the
pynput and evdev backends in :mod:`whisper_dictate.vp_keys`.

Two rules (only active for a bare-modifier PTT key — otherwise behaviour is
completely unchanged):

  1. **No activation inside a chord.** If any OTHER key is already held when the
     PTT key goes down, do not start recording (the user is mid-shortcut).
  2. **Abort on chord formed during hold.** If recording started from a bare
     modifier and any OTHER key goes down before the PTT key is released
     (e.g. Ctrl held, then C → Ctrl+C), cancel: discard the audio, no
     transcription, no injection.

The guard tracks *every* key currently held (opaque tokens — pynput Key/str or
evdev int codes), so it can tell whether a foreign key is present. Key-repeat
(a held key re-firing) never counts as a new key; releases of keys never seen
pressed are ignored.
"""
from __future__ import annotations

# pynput key names that are bare modifiers. A PTT key is "solo-guarded" only
# when key_names is exactly one of these. Generic 'ctrl'/'shift'/'alt' aren't
# normally produced by config, but are accepted for robustness.
_BARE_MODIFIER_NAMES = frozenset({
    "ctrl", "ctrl_l", "ctrl_r",
    "shift", "shift_l", "shift_r",
    "alt", "alt_l", "alt_r", "alt_gr",
    "cmd", "cmd_l", "cmd_r",
    "win", "win_l", "win_r",
    "super", "super_l", "super_r",
    "meta", "meta_l", "meta_r",
})


def is_bare_modifier_key(key_names: list[str]) -> bool:
    """True when the configured PTT key is a single bare-modifier key.

    ``key_names`` is the split form of the ``key`` setting (``a+b`` → two
    names). A multi-key chord, or any single non-modifier key (scroll_lock,
    f9, …), is NOT solo-guarded.
    """
    return len(key_names) == 1 and key_names[0] in _BARE_MODIFIER_NAMES


class SoloModifierGuard:
    """Tracks held keys to enforce the bare-modifier "press alone" rules.

    Backend-agnostic: keys are opaque, hashable tokens (pynput Key/str for the
    pynput backend, evdev int keycodes for evdev). ``target`` is the single PTT
    key token; it never counts as "another key".

    When ``enabled`` is False (PTT key is not a bare modifier) every method is a
    no-op that preserves the original behaviour.
    """

    def __init__(self, target, *, enabled: bool) -> None:
        self.enabled = enabled
        self._target = target
        self._held: set = set()

    # --- press / release tracking -------------------------------------------------
    def note_press(self, key) -> bool:
        """Record ``key`` as held. Returns True if this is a *newly* held key.

        Key-repeat (the key is already in the held set) returns False so a held
        key re-firing is never treated as a new keypress.
        """
        if key in self._held:
            return False
        self._held.add(key)
        return True

    def note_release(self, key) -> None:
        """Record ``key`` as released. Releases of unknown keys are ignored."""
        self._held.discard(key)

    # --- the two rules -------------------------------------------------------------
    def foreign_key_held(self) -> bool:
        """True if any key other than the PTT target is currently held."""
        return any(k != self._target for k in self._held)

    def may_start_on_target_down(self) -> bool:
        """Rule 1: may we start recording now that the PTT key just went down?

        For a guarded bare-modifier key, refuse if any other key is already
        held (the user is forming a chord). When not guarded, always allow.
        """
        if not self.enabled:
            return True
        return not self.foreign_key_held()

    def should_cancel_on_press(self, key) -> bool:
        """Rule 2: did a NEW foreign key go down while recording from the PTT key?

        Returns True when ``key`` is a freshly-held key other than the target —
        i.e. a chord (Ctrl+C) has formed and dictation must be discarded. Always
        False when not guarded.
        """
        if not self.enabled:
            return False
        return key != self._target and self.note_press(key)
