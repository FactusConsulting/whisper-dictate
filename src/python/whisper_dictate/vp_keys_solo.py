"""Bare-modifier "press alone" semantics for the push-to-talk key.

When the configured PTT binding is made up ENTIRELY of *bare modifiers* (ctrl /
shift / alt / win in any left/right variant) — whether a single modifier
(``ctrl_l``) or a modifier chord (``shift_r+ctrl_r``, ``alt_l+shift_l``) — users
expect it to dictate ONLY when that exact combo is pressed and nothing else, not
as part of a larger OS/app shortcut like Shift+Ctrl+X. This module holds the
small, backend-agnostic state machine that enforces that, shared by both the
pynput and evdev backends in :mod:`whisper_dictate.vp_keys`.

The guard tracks a SET of target keys (the modifiers that make up the binding);
any key outside that set is "foreign". Two rules (only active for an all-bare-
modifier binding — otherwise behaviour is completely unchanged):

  1. **No activation inside a larger chord.** At the moment the binding completes
     (the last target key goes down), if any FOREIGN key (one outside the target
     set) is already held, do not start recording (the user is mid-shortcut).
     Holding one target modifier then pressing the rest of the chord is fine.
  2. **Abort on a foreign key during hold.** If recording started from the
     modifier binding and any FOREIGN key goes down before the binding is
     released (e.g. Ctrl held, then C → Ctrl+C; or X added to a held Shift+Ctrl),
     cancel: discard the audio, no transcription, no injection.

The guard tracks *every* key currently held (opaque tokens — pynput Key/str or
evdev int codes), so it can tell whether a foreign key is present. Key-repeat
(a held key re-firing) never counts as a new key; releases of keys never seen
pressed are ignored.

**Phantom-held-key self-healing.** Global hooks routinely miss key-ups: Alt+Tab
eats the Alt-up, Win+L / secure-desktop / RDP focus loss drop events, pynput
suppression can swallow them. A held entry that is never released would make
``foreign_key_held()`` True forever, silently wedging bare-modifier PTT off
until the app restarts. To self-heal, each held key carries a monotonic
timestamp; entries older than ``FOREIGN_KEY_EXPIRY_S`` are ignored and pruned.
A real chord press lands within ~1 s, so a phantom entry recovers after 10 s;
a foreign key genuinely held for >10 s falsely allowing a start is rare and
benign (recoverable), while a permanent PTT wedge is not. OS key-repeat (pynput
press repeats, evdev value==2 autorepeat) refreshes the timestamp so a key
that is *actually* still held keeps blocking past the nominal expiry.
"""
from __future__ import annotations

import time

# A held foreign key with no observed key-up self-heals after this many seconds
# (see module docstring). A real chord forms within ~1 s, so this is comfortably
# above any genuine chord latency while still recovering from missed key-ups.
FOREIGN_KEY_EXPIRY_S = 10.0

# Key names that are bare modifiers AND resolvable to a real target by at least
# one backend (so the bare-modifier guard can ever actually engage). The set is
# the union of:
#   * pynput Key enum members that are modifiers — generics ctrl/shift/alt/cmd
#     plus the ctrl_l/ctrl_r, shift_l/shift_r, alt_l/alt_r, cmd_l/cmd_r and the
#     alt_gr variants (all real ``pynput.keyboard.Key`` members), resolved via
#     ``getattr(keyboard.Key, kn)`` in ``_pynput_targets``;
#   * evdev names accepted by ``KeyBackendMixin._EVDEV_MAP``: ctrl_l/ctrl_r,
#     shift_l/shift_r, alt_l/alt_r, super_l/super_r.
# Names that NEITHER backend can resolve (win*, meta*, bare super, bare cmd on
# evdev, etc.) are excluded: the target resolver would ``sys.exit`` on them long
# before the guard runs, so advertising them here is misleading dead weight.
_BARE_MODIFIER_NAMES = frozenset({
    # pynput generics (Key.ctrl / Key.shift / Key.alt / Key.cmd all exist)
    "ctrl", "shift", "alt", "cmd",
    # left/right variants resolvable by pynput and/or evdev
    "ctrl_l", "ctrl_r",
    "shift_l", "shift_r",
    "alt_l", "alt_r", "alt_gr",
    "cmd_l", "cmd_r",
    # evdev-only super (KEY_LEFTMETA / KEY_RIGHTMETA)
    "super_l", "super_r",
})


def is_bare_modifier_binding(key_names: list[str]) -> bool:
    """True when the PTT binding is made up ENTIRELY of bare-modifier keys.

    ``key_names`` is the split form of the ``key`` setting (``a+b`` → two
    names). The "press alone" guard engages for any binding of 1..N keys that
    are ALL bare modifiers — a single modifier (``ctrl_l``) or a modifier chord
    (``shift_r+ctrl_r``, ``alt_l+shift_l``). A binding containing any
    non-modifier key (``ctrl+f9``, ``f9``) is NOT solo-guarded: its behaviour is
    completely unchanged.
    """
    return bool(key_names) and all(kn in _BARE_MODIFIER_NAMES for kn in key_names)


# Back-compat alias: the original predicate only accepted a single bare modifier.
# Generalised to whole-binding above; keep the old name working for any external
# callers (none in-tree at time of writing).
is_bare_modifier_key = is_bare_modifier_binding


class SoloModifierGuard:
    """Tracks held keys to enforce the bare-modifier "press alone" rules.

    Backend-agnostic: keys are opaque, hashable tokens (pynput Key/str for the
    pynput backend, evdev int keycodes for evdev). ``targets`` is the SET of PTT
    key tokens (one for a solo modifier, several for a modifier chord); none of
    them ever count as "another key" / foreign — only keys outside this set do.

    Accepts either a set/iterable of target tokens or a single token (wrapped in
    a one-element set) for convenience / back-compat.

    When ``enabled`` is False (PTT binding is not all bare modifiers) every
    method is a no-op that preserves the original behaviour.

    Held keys are stored with a monotonic timestamp so stale entries from missed
    key-ups self-heal (see module docstring). ``_now`` is injectable so tests can
    drive the expiry clock deterministically.
    """

    def __init__(self, targets, *, enabled: bool, _now=time.monotonic) -> None:
        self.enabled = enabled
        # Normalise to a set of target tokens. A single token (the common solo
        # case, and the historical signature) is wrapped; ``None`` → empty set.
        if targets is None:
            self._targets: set = set()
        elif isinstance(targets, (set, frozenset, list, tuple)):
            self._targets = set(targets)
        else:
            self._targets = {targets}
        # key token -> monotonic timestamp of the most recent press/repeat.
        self._held: dict = {}
        self._now = _now

    # --- press / release tracking -------------------------------------------------
    def note_press(self, key) -> bool:
        """Record ``key`` as held. Returns True if this is a *newly* held key.

        Key-repeat (the key is already in the held set) returns False so a held
        key re-firing is never treated as a new keypress — but it refreshes the
        timestamp so a genuinely-held key does not expire out of the held set.

        No-op (returns False) when ``enabled`` is False — the guard is inert.
        """
        if not self.enabled:
            return False
        if key in self._held:
            self._held[key] = self._now()  # OS key-repeat: refresh, still not new
            return False
        self._held[key] = self._now()
        return True

    def note_repeat(self, key) -> None:
        """Refresh the timestamp of an already-held key (OS autorepeat).

        For evdev value==2 autorepeat events: keeps a genuinely-held foreign key
        from expiring out of the held set. No-op if the key is not tracked,
        and inert (like every other method) when ``enabled`` is False.
        """
        if not self.enabled:
            return
        if key in self._held:
            self._held[key] = self._now()

    def note_release(self, key) -> None:
        """Record ``key`` as released. Releases of unknown keys are ignored.

        No-op when ``enabled`` is False — the guard is inert.
        """
        if not self.enabled:
            return
        self._held.pop(key, None)

    # --- the two rules -------------------------------------------------------------
    def foreign_key_held(self) -> bool:
        """True if any *non-stale* key outside the PTT target set is held.

        Also prunes entries older than ``FOREIGN_KEY_EXPIRY_S`` so a missed
        key-up cannot wedge bare-modifier PTT off permanently.
        """
        now = self._now()
        stale = [k for k, ts in self._held.items()
                 if now - ts > FOREIGN_KEY_EXPIRY_S]
        for k in stale:
            del self._held[k]
        return any(k not in self._targets for k in self._held)

    def may_start_on_target_down(self) -> bool:
        """Rule 1: may we start recording now that the PTT chord just completed?

        For a guarded bare-modifier binding, refuse if any key OUTSIDE the
        target set is already held (the user is forming a larger chord). Holding
        one target modifier then pressing the rest of the chord is fine — those
        are targets, not foreign. When not guarded, always allow.
        """
        if not self.enabled:
            return True
        return not self.foreign_key_held()

    def should_cancel_on_press(self, key) -> bool:
        """Rule 2: did a NEW foreign key go down while recording from the chord?

        Returns True when ``key`` is a freshly-held key outside the target set —
        i.e. a foreign key (Ctrl+C, or X added to Shift+Ctrl) has joined the
        held modifier(s) and dictation must be discarded. Always False when not
        guarded.
        """
        if not self.enabled:
            return False
        return key not in self._targets and self.note_press(key)
