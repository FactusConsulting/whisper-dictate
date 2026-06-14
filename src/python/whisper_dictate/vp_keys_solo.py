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

**Side-specific modifier matching (reverses #254).** Modifiers are matched
SIDE-SPECIFICALLY: a binding of ``ctrl_l`` is satisfied by left Ctrl, NOT by
right Ctrl. The earlier #254 design collapsed every left/right/generic variant
to one family token so both sides triggered — the user has explicitly asked to
reverse that. To keep PTT reliable despite pynput's Windows backend
intermittently delivering the GENERIC ``Key.ctrl`` (side unknown) instead of the
specific ``Key.ctrl_l`` (pynput #15/#33/#139/#155), the single side-aware
predicate :func:`modifier_matches` keeps a GENERIC FALLBACK: a side-specific
target is satisfied by its own side OR by the generic family press, but never by
the opposite side. Every matching site (chord start/completion, this guard's
target/foreign test, the quit-key match, both backends) routes through it.

Residual reliability tradeoff (preserved-but-narrowed vs #254): with a
side-specific binding, (a) if pynput delivers the OPPOSITE specific side the
chord will NOT start (rare; the accepted cost of side-specificity), and (b) a
press of the other side that the OS happens to report AS the generic family
token still matches (rare leak, fail-safe toward starting). evdev is unaffected —
its integer scancodes are already unambiguously side-specific (KEY_LEFTCTRL !=
KEY_RIGHTCTRL) with no generic variant, so each evdev press matches its bound
side exactly.

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
from collections import abc as _abc

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


# Modifier families: map every pynput modifier variant name (``ctrl_l`` /
# ``ctrl_r`` / generic ``ctrl`` …) to its family token (``ctrl``). Used to decide
# whether two names belong to the same modifier family and whether a name is a
# specific *side* (``ctrl_l``/``ctrl_r``) versus the generic family name
# (``ctrl``). pynput exposes ``alt_gr`` as part of the alt family.
_MODIFIER_FAMILIES = {
    "ctrl": "ctrl", "ctrl_l": "ctrl", "ctrl_r": "ctrl",
    "shift": "shift", "shift_l": "shift", "shift_r": "shift",
    "alt": "alt", "alt_l": "alt", "alt_r": "alt", "alt_gr": "alt",
    "cmd": "cmd", "cmd_l": "cmd", "cmd_r": "cmd",
}

# The bare family names (no side). A binding/press carrying one of these is the
# OS-reported GENERIC modifier whose side is unknown.
_GENERIC_MODIFIER_NAMES = frozenset({"ctrl", "shift", "alt", "cmd"})


def canon_modifier(k):
    """Collapse a pynput modifier ``Key`` to a side-insensitive family token.

    Returns a stable family string (``"ctrl"``/``"shift"``/``"alt"``/``"cmd"``)
    for any left/right/generic modifier ``Key`` (identified by its ``.name``).
    Every other token — letters / KeyCodes, function keys, the quit key, media
    keys, and plain string tokens — is returned UNCHANGED. Idempotent.

    .. note::
       Since the side-specific matching change (PR reversing #254's full
       side-insensitivity) this is **no longer used to compare/collapse keys for
       PTT chord matching** — that goes through :func:`modifier_matches`, which is
       side-aware. ``canon_modifier`` survives only where a genuine FAMILY token
       is wanted: capture (:func:`whisper_dictate.vp_keys_capture.key_to_setting_name`)
       uses it to recognise that a captured key is *some* modifier and to derive
       the generic family name when only a generic press was ever observed.
    """
    name = getattr(k, "name", None)  # pynput Key enum member
    if isinstance(name, str):
        family = _MODIFIER_FAMILIES.get(name)
        if family is not None:
            return family
    return k


def modifier_family(k):
    """The modifier FAMILY token (``"ctrl"`` …) for a key, or ``None``.

    Returns the family for any left/right/generic modifier key (by its name);
    ``None`` for non-modifier keys (letters, function keys, the quit key) and for
    tokens with no resolvable name. Used to decide whether two tokens are the
    same physical modifier reported under different variants (press ``ctrl_l`` /
    release generic ``ctrl``) so a release reliably breaks the chord.
    """
    name = key_name(k)
    if name is None:
        return None
    return _MODIFIER_FAMILIES.get(name)


def key_name(k):
    """Best-effort modifier/key NAME for a pressed token, or ``None``.

    * pynput ``Key`` enum member → its ``.name`` (``"ctrl_l"``, ``"ctrl"``,
      ``"f9"`` …).
    * a plain string token (the tokens the unit tests and some call sites use,
      e.g. ``"ctrl_l"``) → itself.
    Anything else (a letter ``KeyCode``, an evdev int code, a media Key) → the
    pynput ``.name`` if present, otherwise ``None`` — callers that need a name to
    do side-aware modifier matching simply get no match.
    """
    name = getattr(k, "name", None)  # pynput Key enum member
    if isinstance(name, str):
        return name
    if isinstance(k, str):
        return k
    return None


def modifier_matches(pressed_key, target_name) -> bool:
    """Side-aware match: does ``pressed_key`` satisfy a binding of ``target_name``?

    This is the single predicate every PTT matching site routes through (chord
    start/completion, the SoloModifierGuard target/foreign test, and the quit-key
    comparison). It REVERSES the full side-insensitivity of #254 — left and right
    modifiers are now distinct — while keeping a GENERIC fallback so reliability
    is preserved.

    ``pressed_key`` is an opaque backend token (a pynput ``Key``/``KeyCode``, a
    plain string token, an evdev int — anything :func:`key_name` can name);
    ``target_name`` is the PTT ``key`` setting name for one chord member
    (``"ctrl_l"``, ``"ctrl_r"``, the generic ``"ctrl"``, ``"f9"`` …).

    Matching rules:

    * **Side-specific target** (``ctrl_l``): satisfied by the SAME specific side
      (``Key.ctrl_l``) OR by the GENERIC family press (``Key.ctrl``) whose side
      the OS did not report — a fail-safe so the chord still starts when pynput
      intermittently delivers the generic variant. NOT satisfied by the OPPOSITE
      specific side (``Key.ctrl_r``).
    * **Generic target** (bare ``ctrl`` — only if the user ever binds a sideless
      modifier): matches ANY variant of that family (``ctrl_l``/``ctrl_r``/
      generic), i.e. side-insensitive within the family.
    * **Non-modifier target** (``f9``, ``space``, a letter, the ``esc`` quit
      key): plain name equality — unchanged behaviour.

    Residual reliability tradeoff (documented for the user): with a side-specific
    binding, (a) if pynput delivers the OPPOSITE specific side it will NOT match
    (rare; "PTT might not start" — the accepted cost of side-specific matching),
    and (b) a press of the other side that the OS happens to deliver AS the
    generic family token WILL match (rare leak, fail-safe toward starting).
    """
    if not isinstance(target_name, str):
        return False
    pname = key_name(pressed_key)
    if pname is None:
        return False
    # Non-modifier (or unknown) target: exact name equality, nothing fancy.
    target_family = _MODIFIER_FAMILIES.get(target_name)
    if target_family is None:
        return pname == target_name
    pressed_family = _MODIFIER_FAMILIES.get(pname)
    if pressed_family != target_family:
        return False  # different modifier family (ctrl vs shift) — never matches
    # Same family. Decide on sides.
    if target_name in _GENERIC_MODIFIER_NAMES:
        return True  # generic target: any side / generic press of the family
    # Side-specific target (ctrl_l / ctrl_r): same exact side, or the generic
    # family press (side unknown → fail-safe match). The opposite side fails.
    return pname == target_name or pname in _GENERIC_MODIFIER_NAMES


# Media / consumer-control keys the solo guard must IGNORE entirely. A Bluetooth
# headset (e.g. Jabra) can emit consumer-control events — volume up/down/mute,
# play/pause, track next/prev — to the OS while a bare-modifier PTT key is held.
# These are NOT intentional chord-forming keys: they must neither block a start
# (rule 1) nor cancel an in-flight dictation (rule 2), and are never tracked as
# held / counted as foreign. See ``is_ignored_foreign_key``.
#
# pynput represents these as ``Key`` enum members named ``media_*`` (the .name
# attribute is ``media_volume_up`` etc.). evdev delivers raw int keycodes, so we
# match against the corresponding ``ecodes`` integers, resolved lazily below.
_IGNORED_PYNPUT_NAME_PREFIXES = ("media_",)

# evdev ecode *names* for consumer/media keys. Resolved to integer codes once,
# lazily, the first time the predicate sees an int (evdev may be unavailable, in
# which case the set stays empty and nothing is ignored on that backend — but
# evdev is only ever the active backend when it is importable).
_IGNORED_EVDEV_ECODE_NAMES = (
    "KEY_VOLUMEUP", "KEY_VOLUMEDOWN", "KEY_MUTE",
    "KEY_PLAYPAUSE", "KEY_PLAY", "KEY_PAUSE",
    "KEY_NEXTSONG", "KEY_PREVIOUSSONG",
    "KEY_STOPCD", "KEY_PLAYCD", "KEY_PAUSECD",
    "KEY_FORWARD", "KEY_REWIND", "KEY_PLAYER",
)

# Populated lazily by ``_ignored_evdev_codes``; None means "not yet resolved".
_IGNORED_EVDEV_CODES: frozenset[int] | None = None


def _ignored_evdev_codes() -> frozenset[int]:
    """Resolve (once) the set of evdev integer keycodes the guard ignores.

    Robust if evdev is not importable: returns an empty set, so the predicate
    simply never matches an int (no media key gets ignored, original behaviour).
    """
    global _IGNORED_EVDEV_CODES
    if _IGNORED_EVDEV_CODES is None:
        codes: set[int] = set()
        try:
            from evdev import ecodes  # type: ignore
            for name in _IGNORED_EVDEV_ECODE_NAMES:
                code = getattr(ecodes, name, None)
                if isinstance(code, int):
                    codes.add(code)
        except Exception:
            pass
        _IGNORED_EVDEV_CODES = frozenset(codes)
    return _IGNORED_EVDEV_CODES


def is_ignored_foreign_key(key) -> bool:
    """True for media / consumer-control keys the solo guard must ignore.

    Works for BOTH backends:
      * pynput: a ``Key`` enum member whose ``.name`` starts with ``media_``
        (``media_volume_up``/``media_volume_down``/``media_volume_mute``,
        ``media_play_pause``, ``media_next``, ``media_previous``).
      * evdev: a raw integer keycode in the consumer/media ecode set
        (``KEY_VOLUMEUP``/``KEY_VOLUMEDOWN``/``KEY_MUTE``/``KEY_PLAYPAUSE``/
        ``KEY_NEXTSONG``/``KEY_PREVIOUSSONG``/...), resolved lazily.

    Anything else (letters, real modifiers, function keys, the PTT targets) is
    NOT ignored — so genuine foreign keys still cancel and chord bindings are
    unaffected.
    """
    name = getattr(key, "name", None)  # pynput Key enum member
    if isinstance(name, str) and name.startswith(_IGNORED_PYNPUT_NAME_PREFIXES):
        return True
    if isinstance(key, int):
        return key in _ignored_evdev_codes()
    return False


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

    **Target membership.** A held key counts as a target (never foreign) when
    ``is_target(key)`` is True. By default that is plain set membership over the
    target tokens — exactly side-specific for evdev (distinct int scancodes) and
    for the opaque-token unit tests. The pynput backend injects a SIDE-AWARE
    predicate built on :func:`modifier_matches`, so a chord member arriving as the
    GENERIC family variant still counts as its target (fail-safe) while the
    OPPOSITE specific side counts as foreign — see ``_PynputListener``.
    """

    def __init__(self, targets, *, enabled: bool, is_target=None,
                 _now=time.monotonic) -> None:
        self.enabled = enabled
        # Normalise to a set of target tokens. A single token (the common solo
        # case, and the historical signature) is wrapped; ``None`` → empty set.
        if targets is None:
            self._targets: set = set()
        elif isinstance(targets, str):
            # str is iterable but always means ONE key token, never a set of
            # one-character tokens.
            self._targets = {targets}
        elif isinstance(targets, _abc.Iterable):
            self._targets = set(targets)
        else:
            self._targets = {targets}
        # Predicate deciding whether a held key is one of the PTT targets. The
        # default is exact set membership (side-specific for evdev int codes and
        # for opaque-token tests); the pynput backend passes a side-aware matcher.
        self._is_target = is_target if is_target is not None else self._in_targets
        # key token -> monotonic timestamp of the most recent press/repeat.
        self._held: dict = {}
        self._now = _now

    def set_is_target(self, predicate) -> None:
        """Install the target-membership predicate (held key -> bool).

        Public hook for backends that can only build the predicate after the
        guard exists — the pynput listener installs a side-aware matcher bound to
        itself. ``None`` restores the default exact set membership.
        """
        self._is_target = predicate if predicate is not None else self._in_targets

    def _in_targets(self, key) -> bool:
        return key in self._targets

    # --- press / release tracking -------------------------------------------------
    def note_press(self, key) -> bool:
        """Record ``key`` as held. Returns True if this is a *newly* held key.

        Key-repeat (the key is already in the held set) returns False so a held
        key re-firing is never treated as a new keypress — but it refreshes the
        timestamp so a genuinely-held key does not expire out of the held set.

        Media / consumer-control keys (volume, play/pause — see
        ``is_ignored_foreign_key``) are NEVER tracked: a Bluetooth headset
        emitting them while a bare-modifier PTT key is held must not form a
        "foreign key" that blocks a start or cancels a recording. Returns False
        for them (not a new key) and they never enter ``_held``.

        No-op (returns False) when ``enabled`` is False — the guard is inert.
        """
        if not self.enabled:
            return False
        if is_ignored_foreign_key(key):
            return False  # media/consumer key: ignored, not tracked, never foreign
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

        Removes the exact held token AND any held token in the SAME modifier
        family: with side-specific matching pynput can report the press as a
        side-specific token (``ctrl_r``) but the release as the generic family
        token (``ctrl``) — popping only the exact token would leave a phantom
        held key (a foreign key wrongly blocking starts / cancelling) until it
        expires. Non-modifier / evdev int tokens have no family
        (``modifier_family`` → ``None``) and fall back to exact removal.

        No-op when ``enabled`` is False — the guard is inert.
        """
        if not self.enabled:
            return
        self._held.pop(key, None)
        family = modifier_family(key)
        if family is not None:
            for held in [k for k in self._held if modifier_family(k) == family]:
                del self._held[held]

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
        return any(not self._is_target(k) for k in self._held)

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

        Returns True when ``key`` is a freshly-held key that is NOT a target —
        i.e. a foreign key (Ctrl+C, or X added to Shift+Ctrl, or the WRONG-SIDE
        modifier under side-specific matching) has joined the held modifier(s)
        and dictation must be discarded. Always False when not guarded.
        """
        if not self.enabled:
            return False
        if is_ignored_foreign_key(key):
            return False  # media/consumer key never cancels (and note_press would
            # have ignored it anyway — explicit here for intent)
        return not self._is_target(key) and self.note_press(key)
