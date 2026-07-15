"""Press-to-capture hotkey binding: the PURE chord-assembly logic.

Issue #258 part 1. Instead of typing pynput key names into the config, the user
enters a *capture mode*, **presses and holds** the key(s) they want, and on
**release** the app records exactly which keys were held — that set becomes the
push-to-talk chord. "What you press is what you bind."

This module is the BACKEND-AGNOSTIC, side-effect-free half. It contains:

  * :func:`key_to_setting_name` — turn one opaque pynput key token (a ``Key``
    enum member or a ``KeyCode``) into the string the PTT ``key`` setting uses
    (``ctrl_l``, ``ctrl_r``, ``f9``, ``space`` …; character ``KeyCode``s like
    ``a`` are unbindable and yield ``None``), recording the ACTUAL SIDE pressed
    (``Key.ctrl_l`` → ``"ctrl_l"``) so the captured binding is side-specific —
    matching the side-specific PTT matching that reverses #254. Only when pynput
    delivers the sideless GENERIC variant (``Key.ctrl``, no side known) does it
    fall back to a concrete side name (``"ctrl_r"``) — generic family names are
    not resolvable on the evdev backend — so that rare case binds the right side
    (plus any generic-delivered presses); re-capture if you meant the left key.
  * :class:`ChordCapture` — the state machine: feed it synthetic ``press`` /
    ``release`` events; it accumulates the SET of keys held and, on the release
    that empties the held set, emits the canonical chord (sorted, ``+``-joined)
    captured from the *high-water mark* of the hold — i.e. the full set held at
    the moment release began, not just the first key.

The live pynput listener (:mod:`whisper_dictate.vp_keys_capture_io`) is a thin IO
shell that forwards real key events into a :class:`ChordCapture`, so all the
interesting behaviour is unit-tested here without touching a keyboard.

Why high-water mark, not "set at first release": users assemble a chord by
pressing keys one after another (Ctrl down, then Shift down) and then let go —
often releasing them one at a time too. We must bind the FULL chord they built
up, so we snapshot the largest simultaneously-held set and emit it once every key
is back up. A lone tap (press then release of a single key) binds that one key.
"""
from __future__ import annotations

from whisper_dictate.vp_keys_solo import (
    canon_modifier,
    is_ignored_foreign_key,
    modifier_family,
)

# When only the sideless GENERIC modifier variant was ever observed during
# capture (pynput delivered ``Key.ctrl`` with no left/right info), we fall back to
# a CONCRETE, resolvable family name so both backends accept the binding. The
# right-hand variant matches the project default (``ctrl_r``) and keeps the left
# hand free for normal typing. This is only reached for the generic press; a
# side-specific press (``Key.ctrl_l``) is recorded verbatim as ``"ctrl_l"`` so
# the side is preserved (the #254 reversal). ``alt_gr`` has family ``alt`` and so
# lands on ``alt_r`` here — acceptable for a PTT binding.
_GENERIC_FAMILY_TO_SETTING_NAME = {
    "ctrl": "ctrl_r",
    "shift": "shift_r",
    "alt": "alt_r",
    # pynput exposes Win/Cmd as the cmd family; the PTT key resolver maps
    # ``cmd_r`` via getattr(keyboard.Key, "cmd_r"). evdev uses super_r.
    "cmd": "cmd_r",
}


def key_to_setting_name(key) -> str | None:
    """Map one captured pynput key token to its PTT ``key`` setting name.

    Returns the string the ``key`` setting / ``--key`` flag understands, or
    ``None`` for a token we cannot bind (so the capture flow can ignore it):

      * Side-specific modifier ``Key`` (``Key.ctrl_l`` → ``"ctrl_l"``,
        ``Key.ctrl_r`` → ``"ctrl_r"``): the ``.name`` verbatim — the ACTUAL SIDE
        pressed is recorded, so the captured binding is side-specific, matching
        the side-specific PTT matching that reverses #254. ``Key.alt_gr`` is
        recorded as ``"alt_gr"`` (its own resolvable name).
      * Generic modifier ``Key`` (sideless ``Key.ctrl`` — the only case where
        pynput does not tell us which side): there is no side to record, so it
        falls back to a concrete side name (``"ctrl_r"`` …) — generic family
        names aren't resolvable on the evdev backend. Under side-specific
        matching that binds the RIGHT side (and any press the OS reports as the
        generic family token), NOT the left. This is the documented edge case: a
        capture that only ever saw the sideless generic cannot know the real
        side, so it picks one — re-capture if you intended the other side.
      * Other named special ``Key`` (``Key.f9`` → ``"f9"``, ``Key.space`` →
        ``"space"``): the ``.name`` verbatim. The pynput backend resolves these;
        the evdev backend only resolves ``f1``..``f12`` from this group.
    Character ``KeyCode``s (letters like ``a``) and any token with neither a
    resolvable ``.name`` nor a backend mapping yield ``None``: the pynput/evdev
    backends only resolve named ``Key`` members (plus f1..f12 / modifiers on
    evdev), so a single character can't be bound as a PTT key — capture ignores
    it rather than writing an unusable value that would crash at startup
    (``unknown key 'a'``).
    """
    name = getattr(key, "name", None)
    if isinstance(name, str) and name:
        family = modifier_family(key)
        # A sideless generic modifier press (name == its own family, e.g.
        # "ctrl") carries no side: fall back to a concrete resolvable family
        # name. A side-specific modifier (ctrl_l / ctrl_r / alt_gr) and every
        # non-modifier named key keep their own ``.name`` verbatim.
        if family is not None and name == family:
            return _GENERIC_FAMILY_TO_SETTING_NAME.get(family, name)
        return name
    # No usable name. canon_modifier still maps a family STRING token back to a
    # concrete name (defensive: handles a pre-collapsed family token passed in);
    # everything else (letter KeyCodes, raw VK codes) is unbindable → None.
    canon = canon_modifier(key)
    if isinstance(canon, str):
        return _GENERIC_FAMILY_TO_SETTING_NAME.get(canon, canon)
    return None


def canonical_chord(names) -> str:
    """Join already-resolved setting names into the canonical chord string.

    De-duplicates (e.g. left Ctrl and a generic Ctrl that both resolved to the
    same name) and sorts so the binding is stable regardless of press order,
    matching how ``vp_keys`` compares chords as an unordered set. Note that since
    the side-specific change, holding BOTH physical sides of a family (left AND
    right Ctrl, both reported with their side) now yields a two-member chord
    (``ctrl_l+ctrl_r``) rather than collapsing to one — each side is recorded
    verbatim. Empty input yields ``""``.
    """
    return "+".join(sorted(set(names)))


class ChordCapture:
    """Accumulate held keys; emit the canonical chord once they are all released.

    Pure and synchronous: drive it with :meth:`press` / :meth:`release` calls
    carrying opaque pynput key tokens (or any hashable stand-in, as the tests
    do). It tracks the set currently held AND the high-water mark (the largest
    simultaneously-held set), so the captured chord is the FULL set held at the
    moment release began — not merely the first key, and not eroded as the user
    lets go one key at a time.

    Lifecycle:
      * Repeated presses of an already-held key (OS key-repeat) are ignored.
      * :meth:`release` returns the canonical chord string at the transition
        from "something held" to "nothing held"; otherwise ``None``. Once it has
        emitted, :attr:`done` is True and :attr:`result` holds the chord, and all
        further presses/releases are inert.

    ``allow_media`` (default False) is the experimental hook for headset / media
    keys: when False, a media/consumer-control key press is dropped (never enters
    the held set), so a stray Bluetooth volume event can't pollute a keyboard
    chord. When True, media keys are captured like any other key — the cheap,
    clearly-marked scaffold for the headset-button investigation in #258 part 2.
    """

    def __init__(self, *, allow_media: bool = False) -> None:
        self._allow_media = allow_media
        self._held: set = set()
        # _pre_release_snapshot: the held set snapshotted on every press so we
        # always have "the set that was held just before the user started
        # releasing" — even when the chord changes WITHOUT growing in size (e.g.
        # Ctrl+Shift held, Ctrl released, Alt pressed → still size 2, but the
        # held set is now Shift+Alt, not Ctrl+Shift). Snapshotting on every press
        # (rather than only when size increases) ensures the snapshot is always
        # current at the moment the first release lands.
        self._pre_release_snapshot: set = set()
        self.done = False
        self.result: str | None = None

    # --- introspection (handy for a live "keys held: ..." prompt) ----------
    def held_names(self) -> list[str]:
        """Sorted setting-names captured so far (for a live capture display)."""
        return sorted(self._resolved(self._pre_release_snapshot))

    def _resolved(self, keys) -> set:
        out: set = set()
        for k in keys:
            name = key_to_setting_name(k)
            if name is not None:
                out.add(name)
        return out

    def _is_droppable_media(self, key) -> bool:
        # Without allow_media, media/consumer-control keys (volume, play/pause —
        # the same set the solo guard ignores) never join the chord, so a stray
        # Bluetooth event can't pollute a keyboard binding.
        return not self._allow_media and is_ignored_foreign_key(key)

    # --- the state machine -------------------------------------------------
    def press(self, key) -> None:
        """Record ``key`` as held. No-op once a chord has been captured.

        Ignores OS key-repeat (already held) and, unless ``allow_media``, drops
        media/consumer-control keys so they never join a keyboard chord.
        """
        if self.done:
            return
        if self._is_droppable_media(key):
            return
        # Ignore tokens we could never bind, so they don't keep the held set
        # non-empty forever (which would stop release from ever emitting).
        if key_to_setting_name(key) is None:
            return
        self._held.add(key)
        # Snapshot the held set on every press so the snapshot always reflects
        # the most recent "fully assembled" chord — even when the chord changes
        # without growing (e.g. Ctrl+Shift → release Ctrl → press Alt: the held
        # set becomes Shift+Alt, same size, but the content changed). Using
        # every-press snapshotting (not just size-increase) means the snapshot
        # captured when the first release arrives always shows the actual keys
        # the user is holding at that moment.
        self._pre_release_snapshot = set(self._held)

    def release(self, key) -> str | None:
        """Record ``key`` as released; emit the chord when nothing is left held.

        Returns the canonical chord string exactly once, on the release that
        empties the held set (the user has let go of the whole chord). Returns
        ``None`` on every other release (including releases of keys never seen
        pressed, and any release after the chord is already captured).
        """
        if self.done:
            return None
        self._held.discard(key)
        if self._held:
            return None  # still holding part of the chord
        if not self._pre_release_snapshot:
            return None  # release with nothing ever captured — ignore
        chord = canonical_chord(self._resolved(self._pre_release_snapshot))
        if not chord:
            # Everything held resolved to nothing bindable: reset and keep going.
            self._pre_release_snapshot = set()
            return None
        self.done = True
        self.result = chord
        return chord
