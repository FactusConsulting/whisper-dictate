"""Press-to-capture hotkey binding: the PURE chord-assembly logic.

Issue #258 part 1. Instead of typing pynput key names into the config, the user
enters a *capture mode*, **presses and holds** the key(s) they want, and on
**release** the app records exactly which keys were held — that set becomes the
push-to-talk chord. "What you press is what you bind."

This module is the BACKEND-AGNOSTIC, side-effect-free half. It contains:

  * :func:`key_to_setting_name` — turn one opaque pynput key token (a ``Key``
    enum member or a ``KeyCode``) into the string the PTT ``key`` setting uses
    (``ctrl_r``, ``f9``, ``a`` …), collapsing left/right/generic modifier
    variants side-insensitively by reusing
    :func:`whisper_dictate.vp_keys_solo.canon_modifier`.
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

from whisper_dictate.vp_keys_solo import canon_modifier, is_ignored_foreign_key

# A captured modifier collapses (side-insensitively) to a family token; we then
# bind a CONCRETE, resolvable name so both backends accept it. The right-hand
# variant matches the project default (``ctrl_r``) and keeps the left hand free
# for normal typing. ``alt_gr`` collapses to ``alt`` upstream, so it lands on
# ``alt_r`` here too — acceptable for a PTT binding.
_FAMILY_TO_SETTING_NAME = {
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

      * Modifier ``Key`` (``Key.ctrl_l`` / generic ``Key.ctrl`` / …): collapsed
        side-insensitively via :func:`canon_modifier` to a family token, then to
        a concrete resolvable name (``ctrl_r`` …) — so left and right Ctrl bind
        identically, exactly as part 1 of #258 asks.
      * Other named special ``Key`` (``Key.f9`` → ``"f9"``, ``Key.space`` →
        ``"space"``): the ``.name`` verbatim. The pynput backend resolves these;
        the evdev backend only resolves ``f1``..``f12`` from this group.
      * Character ``KeyCode`` (the letter ``a`` → ``"a"``): the ``.char``. Bind
        a single letter as a PTT key if the user really wants to.

    Anything with neither a usable ``.name`` nor a ``.char`` (an unknown
    ``KeyCode`` carrying only a raw virtual-key code) yields ``None``.
    """
    canon = canon_modifier(key)
    # canon_modifier returns a family STRING for modifiers, else the token back.
    if isinstance(canon, str):
        mapped = _FAMILY_TO_SETTING_NAME.get(canon)
        return mapped if mapped is not None else canon
    name = getattr(key, "name", None)
    if isinstance(name, str) and name:
        return name
    char = getattr(key, "char", None)
    if isinstance(char, str) and char:
        return char
    return None


def canonical_chord(names) -> str:
    """Join already-resolved setting names into the canonical chord string.

    De-duplicates (a side-insensitive collapse can map two held keys — left and
    right Ctrl — onto the same ``ctrl_r`` name) and sorts so the binding is
    stable regardless of press order, matching how ``vp_keys`` compares chords as
    an unordered set. Empty input yields ``""``.
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
        self._high_water: set = set()
        self.done = False
        self.result: str | None = None

    # --- introspection (handy for a live "keys held: ..." prompt) ----------
    def held_names(self) -> list[str]:
        """Sorted setting-names captured so far (for a live capture display)."""
        return sorted(self._resolved(self._high_water))

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
        # Snapshot the high-water mark: the largest set held simultaneously.
        if len(self._held) > len(self._high_water):
            self._high_water = set(self._held)

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
        if not self._high_water:
            return None  # release with nothing ever captured — ignore
        chord = canonical_chord(self._resolved(self._high_water))
        if not chord:
            # Everything held resolved to nothing bindable: reset and keep going.
            self._high_water = set()
            return None
        self.done = True
        self.result = chord
        return chord
