"""Unit tests for vp_keys.KeyBackendMixin (extracted from runtime.py).

Global push-to-talk hotkey detection. The evdev/pynput event loops block on real
devices, so these focus on the pieces that are unit-testable without an input
device: key-name resolution, the quit-key fallback, evdev availability probing,
and the run() backend-dispatch decision (Wayland evdev vs X11 pynput vs the
Wayland-without-evdev exit).
"""
import types as _types

from helpers import (
    _env,
    patch,
    sys,
    types,
    unittest,
)

from whisper_dictate import vp_keys
from whisper_dictate import vp_keys_solo


class _FakeMediaKey:
    """Hashable stand-in for a pynput ``Key`` enum member (real Key members are
    hashable). Exposes ``.name`` like ``Key.media_volume_up`` so the solo guard's
    ``is_ignored_foreign_key`` predicate matches on the ``media_`` prefix."""

    def __init__(self, name):
        self.name = name

    def __hash__(self):
        return hash(self.name)

    def __eq__(self, other):
        return isinstance(other, _FakeMediaKey) and other.name == self.name

    def __repr__(self):
        return f"<Key.{self.name}>"


class _FakeModKey:
    """Hashable stand-in for a real pynput modifier ``Key`` enum member.

    Real ``pynput.keyboard.Key`` members (``Key.ctrl_l``, generic ``Key.ctrl``,
    etc.) are hashable singletons exposing a ``.name`` (``"ctrl_l"``, ``"ctrl"``).
    The production ``modifier_matches`` keys off that ``.name`` to match a pressed
    key against a target side-specifically (with a generic fallback), so these
    fakes must compare by name to model the real enum's identity. Two
    ``_FakeModKey`` with the SAME name are equal (one OS may emit ``Key.ctrl``
    twice); different names are distinct (``ctrl_l`` vs ``ctrl`` vs ``ctrl_r``),
    exactly as the real enum behaves — the distinction the side-specific matcher
    now honours (left != right) with a generic-fallback safety net."""

    def __init__(self, name):
        self.name = name

    def __hash__(self):
        return hash(self.name)

    def __eq__(self, other):
        return isinstance(other, _FakeModKey) and other.name == self.name

    def __repr__(self):
        return f"<Key.{self.name}>"


class _FakeCharKey:
    """Hashable stand-in for a pynput ``KeyCode`` of a single character.

    Real ``pynput.keyboard.KeyCode`` (what the listener receives for a letter /
    digit key, e.g. a quit key configured as ``"q"``) exposes a ``.char`` but no
    ``.name``. ``key_name`` must fall back to ``.char`` so a 1-character quit key
    still matches through ``modifier_matches`` (the #274 regression)."""

    def __init__(self, char):
        self.char = char

    def __hash__(self):
        return hash(self.char)

    def __eq__(self, other):
        return isinstance(other, _FakeCharKey) and other.char == self.char

    def __repr__(self):
        return f"<KeyCode char={self.char!r}>"


def _fake_keyboard():
    keyboard = _types.ModuleType("keyboard")
    keyboard.Key = types.SimpleNamespace(
        ctrl_r="<ctrl_r>", shift_r="<shift_r>", alt_r="<alt_r>",
        f9="<f9>", esc="<esc>", f12="<f12>",
    )
    return keyboard


class _Target(vp_keys.KeyBackendMixin):
    def __init__(self, key="ctrl_r", lang="en"):
        self.key = key
        self.lang = lang
        self.started = 0
        self.stopped = 0
        self.cancelled = 0

    def _start(self):
        self.started += 1

    def _stop_and_transcribe(self):
        self.stopped += 1

    def _cancel_and_discard(self, epoch=None):
        self.cancelled += 1


class EvdevMapTests(unittest.TestCase):
    """KeyBackendMixin._EVDEV_MAP must include all keys that capture can emit."""

    def test_cmd_r_in_evdev_map(self):
        # Copilot finding 2: press-to-capture emits "cmd_r" for the Win/Super
        # key; the evdev backend must resolve it without calling sys.exit.
        self.assertIn("cmd_r", vp_keys.KeyBackendMixin._EVDEV_MAP,
                      "cmd_r must be in _EVDEV_MAP so evdev users can bind Win key")
        self.assertEqual(vp_keys.KeyBackendMixin._EVDEV_MAP["cmd_r"], "KEY_RIGHTMETA")

    def test_cmd_l_in_evdev_map(self):
        self.assertIn("cmd_l", vp_keys.KeyBackendMixin._EVDEV_MAP)
        self.assertEqual(vp_keys.KeyBackendMixin._EVDEV_MAP["cmd_l"], "KEY_LEFTMETA")

    def test_alt_gr_in_evdev_map(self):
        # Capture records AltGr as "alt_gr" (its pynput name); evdev must resolve
        # it (to the right Alt) so a captured AltGr binding doesn't sys.exit.
        self.assertIn("alt_gr", vp_keys.KeyBackendMixin._EVDEV_MAP)
        self.assertEqual(vp_keys.KeyBackendMixin._EVDEV_MAP["alt_gr"], "KEY_RIGHTALT")

    def test_evdev_target_codes_resolves_cmd_r(self):
        # End-to-end: _evdev_target_codes must not sys.exit for a cmd_r binding.
        import types as _local_types
        evdev = _local_types.ModuleType("evdev")
        evdev.ecodes = types.SimpleNamespace(KEY_RIGHTMETA=125)
        t = _Target()
        codes = vp_keys.KeyBackendMixin._evdev_target_codes(t, evdev, ["cmd_r"])
        self.assertEqual(codes, {125})


class PynputTargetTests(unittest.TestCase):
    def test_pynput_targets_resolves_each_key(self):
        t = _Target()
        kb = _fake_keyboard()
        targets = vp_keys.KeyBackendMixin._pynput_targets(t, kb, ["ctrl_r", "shift_r"])
        self.assertEqual(targets, {"<ctrl_r>", "<shift_r>"})

    def test_pynput_targets_exits_on_unknown_key(self):
        t = _Target()
        kb = _fake_keyboard()
        with self.assertRaises(SystemExit):
            vp_keys.KeyBackendMixin._pynput_targets(t, kb, ["bogus_key"])

    def test_pynput_quit_key_resolves_named_key(self):
        t = _Target()
        kb = _fake_keyboard()
        with patch.object(vp_keys, "QUIT_KEY", "esc"):
            self.assertEqual(vp_keys.KeyBackendMixin._pynput_quit_key(t, kb), "<esc>")

    def test_pynput_quit_key_accepts_single_char(self):
        t = _Target()
        kb = _fake_keyboard()
        with patch.object(vp_keys, "QUIT_KEY", "q"):
            self.assertEqual(vp_keys.KeyBackendMixin._pynput_quit_key(t, kb), "q")

    def test_pynput_quit_key_exits_on_unknown(self):
        t = _Target()
        kb = _fake_keyboard()
        with patch.object(vp_keys, "QUIT_KEY", "totally-unknown"):
            with self.assertRaises(SystemExit):
                vp_keys.KeyBackendMixin._pynput_quit_key(t, kb)


class ToggleModeEnabledTests(unittest.TestCase):
    """``_toggle_mode_enabled`` reads VOICEPI_TOGGLE via get_value. Patch the
    config loader to {} so the env var is the sole source (no machine config)."""

    def setUp(self):
        self._cfg = patch("whisper_dictate.vp_config.load_config", return_value={})
        self._cfg.start()
        self.addCleanup(self._cfg.stop)

    def test_defaults_off(self):
        with _env(VOICEPI_TOGGLE=None):
            self.assertFalse(vp_keys._toggle_mode_enabled())

    def test_truthy_env_enables(self):
        for value in ("1", "true", "True", "yes", "on"):
            with _env(VOICEPI_TOGGLE=value):
                self.assertTrue(
                    vp_keys._toggle_mode_enabled(), f"expected {value!r} truthy")

    def test_falsey_env_disables(self):
        for value in ("0", "false", "False", "no", "off", ""):
            with _env(VOICEPI_TOGGLE=value):
                self.assertFalse(
                    vp_keys._toggle_mode_enabled(), f"expected {value!r} falsey")


class HaveEvdevTests(unittest.TestCase):
    def test_have_evdev_true_when_importable(self):
        t = _Target()
        fake_evdev = _types.ModuleType("evdev")
        with patch.dict(sys.modules, {"evdev": fake_evdev}):
            self.assertTrue(vp_keys.KeyBackendMixin._have_evdev(t))

    def test_have_evdev_false_when_missing(self):
        t = _Target()
        # Simulate ImportError on `import evdev`.
        with patch.dict(sys.modules, {"evdev": None}):
            self.assertFalse(vp_keys.KeyBackendMixin._have_evdev(t))


class RunDispatchTests(unittest.TestCase):
    def _target_with_spies(self, key="shift_r+ctrl_r"):
        t = _Target(key=key)
        t.evdev_calls = []
        t.pynput_calls = []
        t._run_evdev = lambda names: t.evdev_calls.append(names)
        t._run_pynput = lambda names: t.pynput_calls.append(names)
        return t

    def test_wayland_with_evdev_uses_evdev(self):
        t = self._target_with_spies()
        t._have_evdev = lambda: True
        with _env(WAYLAND_DISPLAY="wayland-0"):
            t.run()
        self.assertEqual(t.evdev_calls, [["shift_r", "ctrl_r"]])
        self.assertEqual(t.pynput_calls, [])

    def test_wayland_without_evdev_exits(self):
        t = self._target_with_spies()
        t._have_evdev = lambda: False
        with _env(WAYLAND_DISPLAY="wayland-0"):
            with self.assertRaises(SystemExit):
                t.run()
        self.assertEqual(t.evdev_calls, [])
        self.assertEqual(t.pynput_calls, [])

    def test_x11_uses_pynput(self):
        t = self._target_with_spies(key="f9")
        t._have_evdev = lambda: True  # irrelevant off Wayland
        with _env(WAYLAND_DISPLAY=None):
            t.run()
        self.assertEqual(t.pynput_calls, [["f9"]])
        self.assertEqual(t.evdev_calls, [])


def _fake_evdev():
    ev = _types.ModuleType("evdev")
    ev.ecodes = types.SimpleNamespace(EV_KEY=1)
    # value 1 = down, 2 = autorepeat (held), 0 = up — matches real evdev.
    ev.KeyEvent = types.SimpleNamespace(key_down=1, key_hold=2, key_up=0)
    return ev


def _event(etype, code, value):
    return types.SimpleNamespace(type=etype, code=code, value=value)


class _ImmediateThread:
    """Stand-in for threading.Thread that runs the target synchronously, so the
    _stop_and_transcribe dispatch is observable without joining a real thread."""

    def __init__(self, target=None, daemon=None, **_):
        self._target = target

    def start(self):
        if self._target:
            self._target()


class EvdevApplyEventTests(unittest.TestCase):
    def setUp(self):
        self.ev = _fake_evdev()
        self.t = _Target()

    def test_press_then_release_drives_start_and_stop(self):
        pressed = set()
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self.t._evdev_apply_event(
                _event(1, 30, self.ev.KeyEvent.key_down), self.ev, {30}, pressed, False)
            self.assertTrue(rec)
            self.assertEqual(self.t.started, 1)
            rec = self.t._evdev_apply_event(
                _event(1, 30, self.ev.KeyEvent.key_up), self.ev, {30}, pressed, rec)
            self.assertFalse(rec)
            self.assertEqual(self.t.stopped, 1)

    def test_non_target_and_non_key_events_are_ignored(self):
        pressed = set()
        rec = self.t._evdev_apply_event(
            _event(1, 99, self.ev.KeyEvent.key_down), self.ev, {30}, pressed, False)
        self.assertFalse(rec)
        rec = self.t._evdev_apply_event(
            _event(2, 30, self.ev.KeyEvent.key_down), self.ev, {30}, pressed, False)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 0)

    def test_chord_requires_all_codes_before_start(self):
        pressed = set()
        rec = self.t._evdev_apply_event(
            _event(1, 30, self.ev.KeyEvent.key_down), self.ev, {30, 31}, pressed, False)
        self.assertFalse(rec)  # only one of two held
        self.assertEqual(self.t.started, 0)
        rec = self.t._evdev_apply_event(
            _event(1, 31, self.ev.KeyEvent.key_down), self.ev, {30, 31}, pressed, rec)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_autorepeat_value_2_is_ignored_in_hold_mode(self):
        # A held key emits value-2 autorepeat events; hold mode must not act on
        # them (start happens once on the value-1 down, stop on the value-0 up).
        pressed = set()
        rec = self.t._evdev_apply_event(
            _event(1, 30, self.ev.KeyEvent.key_down), self.ev, {30}, pressed, False)
        self.assertTrue(rec)
        rec = self.t._evdev_apply_event(
            _event(1, 30, self.ev.KeyEvent.key_hold), self.ev, {30}, pressed, rec)
        rec = self.t._evdev_apply_event(
            _event(1, 30, self.ev.KeyEvent.key_hold), self.ev, {30}, pressed, rec)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)
        self.assertEqual(self.t.stopped, 0)


class EvdevToggleTests(unittest.TestCase):
    def setUp(self):
        self.ev = _fake_evdev()
        self.t = _Target()

    def _down(self, code, pressed, rec, latched):
        return self.t._evdev_apply_event(
            _event(1, code, self.ev.KeyEvent.key_down), self.ev, {30},
            pressed, rec, toggle_mode=True, latched=latched)

    def _hold(self, code, pressed, rec, latched):
        return self.t._evdev_apply_event(
            _event(1, code, self.ev.KeyEvent.key_hold), self.ev, {30},
            pressed, rec, toggle_mode=True, latched=latched)

    def _up(self, code, pressed, rec, latched):
        return self.t._evdev_apply_event(
            _event(1, code, self.ev.KeyEvent.key_up), self.ev, {30},
            pressed, rec, toggle_mode=True, latched=latched)

    def test_toggle_first_press_starts_second_press_stops(self):
        pressed, latched = set(), [False]
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._down(30, pressed, False, latched)
            self.assertTrue(rec)
            self.assertEqual(self.t.started, 1)
            rec = self._up(30, pressed, rec, latched)   # release: no stop
            self.assertTrue(rec)
            self.assertEqual(self.t.stopped, 0)
            rec = self._down(30, pressed, rec, latched)  # second press: stop
            self.assertFalse(rec)
            self.assertEqual(self.t.stopped, 1)

    def test_toggle_autorepeat_does_not_double_trigger(self):
        # value-2 repeats while the key is held must never flip recording.
        pressed, latched = set(), [False]
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._down(30, pressed, False, latched)
            rec = self._hold(30, pressed, rec, latched)
            rec = self._hold(30, pressed, rec, latched)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)
        self.assertEqual(self.t.stopped, 0)


class PynputListenerTests(unittest.TestCase):
    def _listener(self, targets={"<ctrl_r>"}, quit_key="<esc>", toggle_mode=False):
        return vp_keys._PynputListener(
            _Target(), set(targets), quit_key, toggle_mode=toggle_mode)

    def test_press_and_release_toggle_recording(self):
        ln = self._listener()
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            self.assertIsNone(ln.on_press("<ctrl_r>"))
            self.assertEqual(ln._owner.started, 1)
            self.assertTrue(ln._recording)
            ln.on_release("<ctrl_r>")
            self.assertEqual(ln._owner.stopped, 1)
            self.assertFalse(ln._recording)

    def test_hold_mode_key_repeat_does_not_restart(self):
        # A held key emits repeated press events; hold mode must start once and
        # never re-start while the key stays down.
        ln = self._listener()
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("<ctrl_r>")
            ln.on_press("<ctrl_r>")   # key-repeat
            ln.on_press("<ctrl_r>")   # key-repeat
        self.assertEqual(ln._owner.started, 1)
        self.assertEqual(ln._owner.stopped, 0)
        self.assertTrue(ln._recording)

    def test_toggle_mode_first_press_starts_second_press_stops(self):
        ln = self._listener(toggle_mode=True)
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            # First press: start.
            self.assertIsNone(ln.on_press("<ctrl_r>"))
            self.assertEqual(ln._owner.started, 1)
            self.assertTrue(ln._recording)
            # Release does NOT stop in toggle mode.
            ln.on_release("<ctrl_r>")
            self.assertEqual(ln._owner.stopped, 0)
            self.assertTrue(ln._recording)
            # Second press: stop and transcribe.
            self.assertIsNone(ln.on_press("<ctrl_r>"))
            self.assertEqual(ln._owner.stopped, 1)
            self.assertFalse(ln._recording)

    def test_toggle_mode_key_repeat_does_not_double_trigger(self):
        # While the chord is held, repeated press events must not flip recording
        # back and forth — only the rising edge (first complete press) acts.
        ln = self._listener(toggle_mode=True)
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("<ctrl_r>")   # rising edge: start
            ln.on_press("<ctrl_r>")   # repeat: ignored
            ln.on_press("<ctrl_r>")   # repeat: ignored
        self.assertEqual(ln._owner.started, 1)
        self.assertEqual(ln._owner.stopped, 0)
        self.assertTrue(ln._recording)

    def test_toggle_mode_chord_requires_all_keys(self):
        # Chord toggle: only fires once BOTH keys are held, and re-arms after the
        # chord breaks so the next complete press toggles again.
        ln = self._listener(targets={"<shift_r>", "<ctrl_r>"}, toggle_mode=True)
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("<shift_r>")   # partial chord: nothing
            self.assertEqual(ln._owner.started, 0)
            ln.on_press("<ctrl_r>")     # chord complete: start
            self.assertEqual(ln._owner.started, 1)
            self.assertTrue(ln._recording)
            ln.on_release("<ctrl_r>")   # chord breaks (re-arm), no stop
            ln.on_release("<shift_r>")
            self.assertEqual(ln._owner.stopped, 0)
            self.assertTrue(ln._recording)
            ln.on_press("<shift_r>")
            ln.on_press("<ctrl_r>")     # chord complete again: stop
            self.assertEqual(ln._owner.stopped, 1)
            self.assertFalse(ln._recording)

    def test_non_target_key_press_does_not_accumulate_in_held(self):
        # Comment 1 fix: non-target keys must never be added to the held-chord
        # set, so it stays bounded (only target keys tracked for completion).
        ln = self._listener()
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            ln.on_press("<shift_l>")   # foreign key — not a target
            ln.on_release("<shift_l>")
        self.assertNotIn("<shift_l>", ln._held_keys)
        self.assertEqual(len(ln._held_keys), 0)

    def test_quit_key_held_blocks_bare_modifier_start(self):
        # Comment 2 fix: quit key held → pressing PTT modifier must NOT start
        # dictation (quit key is a foreign held key for rule-1 purposes).
        guard = vp_keys_solo.SoloModifierGuard("<ctrl_l>", enabled=True)
        ln = vp_keys._PynputListener(
            _Target(), {"<ctrl_l>"}, "<esc>", toggle_mode=False, solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            ln.on_press("<esc>")       # quit key — noted in guard but returns early
            ln.on_press("<ctrl_l>")    # PTT modifier: guard must see quit key held
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_quit_key_never_starts_recording(self):
        ln = self._listener()
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            self.assertIsNone(ln.on_press("<esc>"))
        self.assertEqual(ln._owner.started, 0)

    def test_quit_chord_stops_listener_after_threshold(self):
        ln = self._listener()
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            self.assertIsNone(ln.on_press("<esc>"))   # 1st press: below threshold
            self.assertFalse(ln.on_press("<esc>"))     # 2nd press: stop listener

    def test_non_quit_key_resets_quit_streak(self):
        ln = self._listener()
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            ln.on_press("<esc>")          # streak = 1
            ln.on_press("<ctrl_r>")        # resets streak
            self.assertIsNone(ln.on_press("<esc>"))  # streak back to 1, not a stop


class ModifierQuitKeyTests(unittest.TestCase):
    """Copilot finding (PR #254): when VOICEPI_QUIT_KEY is configured to a
    modifier (e.g. ``ctrl``), the quit comparison must be consistent with
    canonicalisation.  Before the fix ``_quit_key`` was stored raw but incoming
    keys were canonicalised *before* the post-_quit_chord equality check, so
    ``"ctrl" == Key.ctrl`` evaluated False and the modifier quit key was never
    recognised — it fell through to ``_solo.note_press`` and potentially joined
    the PTT state machine as a foreign key instead.  The fix canonicalises
    ``self._quit_key`` at construction AND inside ``_quit_chord``, so every
    comparison is canon-vs-canon."""

    def _ln_with_modifier_quit(self, ptt_target="<shift_l>",
                               quit_key=None, enabled=True):
        """Listener whose quit key is a _FakeModKey (modifier)."""
        if quit_key is None:
            quit_key = _FakeModKey("ctrl")   # generic variant of a modifier family
        guard = vp_keys_solo.SoloModifierGuard(
            {ptt_target}, enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), {ptt_target}, quit_key, toggle_mode=False,
            solo_guard=guard)

    # ------------------------------------------------------------------
    # 1. Modifier quit key: quit-streak still fires
    # ------------------------------------------------------------------
    def test_modifier_quit_key_triggers_quit_streak(self):
        # VOICEPI_QUIT_KEY=ctrl: pressing the quit key (as generic Key.ctrl) twice
        # must stop the listener (return False), not be silently ignored.
        ln = self._ln_with_modifier_quit()
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            result_1 = ln.on_press(_FakeModKey("ctrl"))   # streak = 1
            self.assertIsNone(result_1)                   # below threshold
            result_2 = ln.on_press(_FakeModKey("ctrl"))   # streak = 2 → quit
        self.assertFalse(result_2, "modifier quit key should stop the listener")

    def test_single_char_quit_key_triggers_quit_streak(self):
        # #274 regression: the quit match routes through modifier_matches/
        # key_name. A 1-char quit key (VOICEPI_QUIT_KEY="q", stored by
        # _pynput_quit_key as the bare string "q") arrives as a KeyCode with
        # .char but no .name; without key_name's .char fallback the quit key
        # would never match. Two presses within the window must stop the listener.
        ln = self._ln_with_modifier_quit(quit_key="q")
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            r1 = ln.on_press(_FakeCharKey("q"))   # streak = 1
            self.assertIsNone(r1)                  # below threshold
            r2 = ln.on_press(_FakeCharKey("q"))   # streak = 2 → quit
        self.assertFalse(r2, "single-char quit key must trigger the quit streak")

    def test_modifier_quit_key_left_variant_also_triggers(self):
        # A side-specific variant of the modifier quit key (ctrl_l delivered for
        # a quit key configured as "ctrl") is canonicalised to "ctrl" and must
        # still hit the quit streak — not mis-routed to the PTT machinery.
        ln = self._ln_with_modifier_quit()
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            ln.on_press(_FakeModKey("ctrl_l"))   # left variant, canonicalised → ctrl
            result = ln.on_press(_FakeModKey("ctrl_r"))   # right variant → also ctrl
        self.assertFalse(result, "side-specific modifier quit key must also trigger")

    def test_modifier_quit_key_never_starts_recording(self):
        # The quit key must never start dictation, even when the listener's PTT
        # target happens to be a different modifier — the quit key is always
        # treated as foreign and returned early (None, no start).
        ln = self._ln_with_modifier_quit()
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            result = ln.on_press(_FakeModKey("ctrl"))
        self.assertIsNone(result)
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_modifier_quit_key_does_not_join_ptt_chord(self):
        # The quit modifier must not be added to the held-chord set (PTT chord
        # tracking) or influence recording state.  With the bug the key landed in
        # the chord machinery and could, with a matching target, mis-start.
        ln = self._ln_with_modifier_quit(ptt_target="ctrl",
                                         quit_key=_FakeModKey("ctrl"))
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            ln.on_press(_FakeModKey("ctrl"))
        self.assertNotIn("ctrl", ln._held_keys)
        self.assertEqual(ln._owner.started, 0)

    # ------------------------------------------------------------------
    # 2. Non-modifier quit key (esc, f12): behaviour unchanged
    # ------------------------------------------------------------------
    def test_non_modifier_quit_key_esc_still_works(self):
        # Default case: quit key is ``esc`` (a non-modifier). canon is a no-op
        # for it, so the fix must leave the existing behaviour fully intact.
        guard = vp_keys_solo.SoloModifierGuard("<shift_l>", enabled=True)
        ln = vp_keys._PynputListener(
            _Target(), {"<shift_l>"}, "<esc>", toggle_mode=False,
            solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            ln.on_press("<esc>")                        # streak = 1
            result = ln.on_press("<esc>")               # streak = 2 → quit
        self.assertFalse(result)

    def test_non_modifier_quit_key_never_starts_recording(self):
        guard = vp_keys_solo.SoloModifierGuard("<shift_l>", enabled=True)
        ln = vp_keys._PynputListener(
            _Target(), {"<shift_l>"}, "<esc>", toggle_mode=False,
            solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            result = ln.on_press("<esc>")
        self.assertIsNone(result)
        self.assertEqual(ln._owner.started, 0)

    # ------------------------------------------------------------------
    # 3. Generic-fallback reliability is preserved alongside the quit path
    # ------------------------------------------------------------------
    def test_generic_fallback_chord_still_starts(self):
        # Side-specific matching keeps the generic fallback: a shift_l+ctrl_l
        # binding completed by the GENERIC ctrl variant (side unknown) must still
        # start recording, even with a modifier-orthogonal quit key.
        shift_l = _FakeModKey("shift_l")
        ctrl_l = _FakeModKey("ctrl_l")
        targets = {shift_l, ctrl_l}
        guard = vp_keys_solo.SoloModifierGuard(set(targets), enabled=True)
        # Use a non-modifier quit key so the quit path is orthogonal.
        ln = vp_keys._PynputListener(
            _Target(), set(targets), "<esc>", toggle_mode=False,
            solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl"))   # generic ctrl variant (fallback)
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)


class SoloModifierGuardUnitTests(unittest.TestCase):
    """Backend-agnostic state machine in vp_keys_solo."""

    def test_is_bare_modifier_binding(self):
        # Single bare modifier → guarded.
        self.assertTrue(vp_keys_solo.is_bare_modifier_binding(["ctrl_l"]))
        self.assertTrue(vp_keys_solo.is_bare_modifier_binding(["shift_r"]))
        self.assertTrue(vp_keys_solo.is_bare_modifier_binding(["super_l"]))
        # All-bare-modifier CHORDS → now guarded too (the chord extension).
        self.assertTrue(vp_keys_solo.is_bare_modifier_binding(["shift_r", "ctrl_r"]))
        self.assertTrue(vp_keys_solo.is_bare_modifier_binding(["alt_l", "shift_l"]))
        # Any non-modifier in the binding → NOT guarded (unchanged behaviour).
        self.assertFalse(vp_keys_solo.is_bare_modifier_binding(["scroll_lock"]))
        self.assertFalse(vp_keys_solo.is_bare_modifier_binding(["f9"]))
        self.assertFalse(vp_keys_solo.is_bare_modifier_binding(["ctrl_r", "f9"]))
        # Empty binding is never guarded.
        self.assertFalse(vp_keys_solo.is_bare_modifier_binding([]))

    def test_is_bare_modifier_key_backcompat_alias(self):
        # The old single-key predicate name remains a working alias.
        self.assertIs(vp_keys_solo.is_bare_modifier_key,
                      vp_keys_solo.is_bare_modifier_binding)
        self.assertTrue(vp_keys_solo.is_bare_modifier_key(["ctrl_l"]))

    def test_bare_modifier_names_only_backend_resolvable(self):
        # Finding 3: the advertised set must contain only names a backend can
        # actually resolve to a target — names neither backend resolves (win*,
        # meta*, bare super/cmd-on-evdev) would sys.exit before the guard runs.
        names = vp_keys_solo._BARE_MODIFIER_NAMES
        for unresolvable in ("win", "win_l", "win_r", "meta", "meta_l",
                             "meta_r", "super"):
            self.assertNotIn(unresolvable, names)
        # pynput generics + l/r variants and evdev super_l/r ARE resolvable.
        for resolvable in ("ctrl", "shift", "alt", "cmd", "ctrl_l", "ctrl_r",
                           "shift_l", "shift_r", "alt_l", "alt_r", "alt_gr",
                           "cmd_l", "cmd_r", "super_l", "super_r"):
            self.assertIn(resolvable, names)

    def test_disabled_guard_is_noop(self):
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=False)
        # Even with a foreign key noted, a disabled guard always allows start and
        # never cancels.
        g.note_press("shift")
        self.assertTrue(g.may_start_on_target_down())
        self.assertFalse(g.should_cancel_on_press("c"))

    def test_disabled_guard_note_press_does_not_mutate_held(self):
        # Comment 3 fix: note_press must be a true no-op when enabled=False —
        # _held must stay empty so a later enable would start with clean state.
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=False)
        g.note_press("shift")
        g.note_press("alt")
        self.assertEqual(g._held, {})

    def test_disabled_guard_note_release_does_not_mutate_held(self):
        # Comment 4 fix: note_release must also be a true no-op when disabled.
        # Manually insert a key to simulate an edge case, then verify release
        # does not interact with it (the guard is inert).
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=False)
        # note_press is already a no-op when disabled, so _held is empty.
        # Confirm note_release on a key that was never tracked also doesn't crash.
        g.note_release("shift")   # must not raise
        self.assertEqual(g._held, {})

    def test_repeat_press_is_not_new(self):
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        self.assertTrue(g.note_press("ctrl"))
        self.assertFalse(g.note_press("ctrl"))  # key-repeat

    def test_unknown_release_is_ignored(self):
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        g.note_release("never_pressed")  # must not raise / miscount
        self.assertFalse(g.foreign_key_held())

    def test_note_release_generic_clears_held_side_variant(self):
        # Side-specific matching: a modifier pressed as a side-specific token
        # (ctrl_r) but released as the GENERIC family token (ctrl) must still
        # clear the held entry — else it lingers as a phantom foreign key that
        # blocks starts / causes cancels until it expires.
        g = vp_keys_solo.SoloModifierGuard("shift_l", enabled=True)
        g.note_press("ctrl_r")               # a foreign modifier goes down
        self.assertTrue(g.foreign_key_held())
        g.note_release("ctrl")               # OS reports the release as generic
        self.assertEqual(g._held, {})        # family-cleared, no phantom
        self.assertFalse(g.foreign_key_held())

    def test_note_release_side_specific_leaves_opposite_side_held(self):
        # Over-deletion guard: releasing ONE side of a both-sides hold must NOT
        # drop the other side (else a still-held opposite-side foreign key would
        # stop blocking, and a both-side chord would break).
        g = vp_keys_solo.SoloModifierGuard("shift_l", enabled=True)
        g.note_press("ctrl_l")
        g.note_press("ctrl_r")
        g.note_release("ctrl_l")             # release LEFT only
        self.assertIn("ctrl_r", g._held)     # right still held
        self.assertNotIn("ctrl_l", g._held)

    def test_note_release_alt_r_clears_held_alt_gr(self):
        # alt_gr ≡ alt_r (same physical right Alt): a release reported as one
        # spelling clears a hold recorded as the other.
        g = vp_keys_solo.SoloModifierGuard("shift_l", enabled=True)
        g.note_press("alt_gr")
        g.note_release("alt_r")
        self.assertEqual(g._held, {})

    def test_may_start_blocked_when_foreign_held(self):
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        g.note_press("shift")  # shift already down
        self.assertFalse(g.may_start_on_target_down())

    def test_cancel_only_on_new_foreign_key(self):
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        g.note_press("ctrl")
        self.assertFalse(g.should_cancel_on_press("ctrl"))  # the PTT key itself
        self.assertTrue(g.should_cancel_on_press("c"))      # foreign → cancel
        self.assertFalse(g.should_cancel_on_press("c"))     # repeat → no re-cancel

    def test_is_ignored_foreign_key_media_and_consumer(self):
        # (d) Predicate: True for media/consumer keys, False for letters/mods.
        # pynput Key enum members expose a ``.name`` attribute.
        for name in ("media_volume_up", "media_volume_down", "media_volume_mute",
                     "media_play_pause", "media_next", "media_previous"):
            k = _FakeMediaKey(name)
            self.assertTrue(vp_keys_solo.is_ignored_foreign_key(k),
                            f"{name} should be ignored")
        # Real modifiers / letters / function keys are NOT ignored.
        self.assertFalse(vp_keys_solo.is_ignored_foreign_key(_FakeMediaKey("ctrl_l")))
        self.assertFalse(vp_keys_solo.is_ignored_foreign_key(_FakeMediaKey("shift")))
        self.assertFalse(vp_keys_solo.is_ignored_foreign_key("c"))   # pynput char
        self.assertFalse(vp_keys_solo.is_ignored_foreign_key("a"))

    def test_is_ignored_foreign_key_evdev_codes(self):
        # (d) evdev int codes: patch the lazily-resolved code set so the test
        # does not depend on evdev being importable in CI.
        with patch.object(vp_keys_solo, "_IGNORED_EVDEV_CODES",
                          frozenset({115, 114, 113, 164})):  # VOLUP/DN/MUTE/PLAYPAUSE
            self.assertTrue(vp_keys_solo.is_ignored_foreign_key(115))
            self.assertTrue(vp_keys_solo.is_ignored_foreign_key(164))
            self.assertFalse(vp_keys_solo.is_ignored_foreign_key(46))  # 'c' code
            self.assertFalse(vp_keys_solo.is_ignored_foreign_key(29))  # ctrl code

    def test_media_key_does_not_cancel_or_track(self):
        # (a)+(d) at the guard level: holding the PTT modifier, a media key down
        # must NOT cancel and must NOT enter the held set.
        media = _FakeMediaKey("media_volume_up")
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        g.note_press("ctrl")
        self.assertFalse(g.should_cancel_on_press(media))   # no cancel
        self.assertFalse(g.note_press(media))               # not newly held
        self.assertNotIn(media, g._held)                    # never tracked
        self.assertFalse(g.foreign_key_held())              # not foreign

    def test_media_key_held_does_not_block_start(self):
        # (b) A media key "held" must not block a bare-modifier start (rule 1).
        media = _FakeMediaKey("media_play_pause")
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True)
        g.note_press(media)                          # media event arrives first
        self.assertTrue(g.may_start_on_target_down())  # still allowed to start
        g.note_press("ctrl")
        self.assertTrue(g.may_start_on_target_down())

    def test_target_set_chord_treats_all_targets_as_non_foreign(self):
        # Chord generalisation: a SET of targets — none of them count as foreign.
        g = vp_keys_solo.SoloModifierGuard({"shift", "ctrl"}, enabled=True)
        g.note_press("shift")                       # one target held
        self.assertFalse(g.foreign_key_held())      # still no foreign
        g.note_press("ctrl")                        # chord complete (both targets)
        self.assertTrue(g.may_start_on_target_down())
        # A real foreign key joining the chord must cancel.
        self.assertFalse(g.should_cancel_on_press("shift"))  # target, no cancel
        self.assertFalse(g.should_cancel_on_press("ctrl"))   # target, no cancel
        self.assertTrue(g.should_cancel_on_press("x"))       # foreign → cancel

    def test_target_set_blocks_start_when_foreign_held(self):
        # Chord binding: a foreign key held when the chord completes blocks start.
        g = vp_keys_solo.SoloModifierGuard({"shift", "ctrl"}, enabled=True)
        g.note_press("x")                           # foreign held first
        g.note_press("shift")
        g.note_press("ctrl")                        # chord complete, but foreign held
        self.assertFalse(g.may_start_on_target_down())

    def test_phantom_held_key_expires_and_allows_start(self):
        # Finding 1 (a): a foreign key pressed but never released (missed key-up)
        # must self-heal — once the monotonic clock advances past the expiry the
        # phantom entry is ignored/pruned and a bare-modifier start is allowed.
        clock = [1000.0]
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True,
                                           _now=lambda: clock[0])
        g.note_press("shift")               # foreign key down, never released
        self.assertFalse(g.may_start_on_target_down())   # within expiry: blocked
        clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S + 0.1
        self.assertTrue(g.may_start_on_target_down())     # past expiry: allowed
        self.assertNotIn("shift", g._held)                # and pruned

    def test_foreign_key_within_expiry_still_blocks(self):
        # Finding 1 (b): a foreign key held for less than the expiry keeps PTT
        # blocked (a real chord forms within ~1 s).
        clock = [1000.0]
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True,
                                           _now=lambda: clock[0])
        g.note_press("shift")
        clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S - 0.5
        self.assertFalse(g.may_start_on_target_down())

    def test_repeat_refresh_keeps_held_key_blocking_past_expiry(self):
        # Finding 1 (c): a genuinely-held foreign key keeps emitting OS repeats;
        # each refresh resets the timestamp so it keeps blocking well past the
        # nominal expiry window (it is NOT a phantom — it is really held).
        clock = [1000.0]
        g = vp_keys_solo.SoloModifierGuard("ctrl", enabled=True,
                                           _now=lambda: clock[0])
        g.note_press("shift")
        # Advance in sub-expiry steps, refreshing via repeat each time.
        for _ in range(5):
            clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S - 1.0
            g.note_repeat("shift")          # autorepeat refresh
        # Total elapsed >> expiry, but refreshes keep it live → still blocked.
        self.assertFalse(g.may_start_on_target_down())
        # note_press on an already-held key also refreshes (pynput repeat path).
        clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S - 1.0
        self.assertFalse(g.note_press("shift"))   # not newly held
        self.assertFalse(g.may_start_on_target_down())


class SoloGuardSideAwareTests(unittest.TestCase):
    """The SoloModifierGuard accepts an injected side-aware ``is_target``
    predicate (built on modifier_matches by the pynput backend). With it, the
    OPPOSITE specific side counts as a FOREIGN key while the matching side and
    the generic-family fallback count as targets."""

    def _guard(self, target_names, enabled=True):
        def is_target(k):
            return any(vp_keys_solo.modifier_matches(k, n) for n in target_names)
        return vp_keys_solo.SoloModifierGuard(
            set(), enabled=enabled, is_target=is_target)

    def test_matching_side_is_not_foreign(self):
        g = self._guard(["ctrl_l"])
        g.note_press(_FakeModKey("ctrl_l"))
        self.assertFalse(g.foreign_key_held())   # the bound side is a target
        self.assertTrue(g.may_start_on_target_down())

    def test_generic_variant_is_not_foreign(self):
        g = self._guard(["ctrl_l"])
        g.note_press(_FakeModKey("ctrl"))        # generic fallback → target
        self.assertFalse(g.foreign_key_held())
        self.assertTrue(g.may_start_on_target_down())

    def test_opposite_side_is_foreign(self):
        # THE reversal at the guard level: right Ctrl is NOT the bound target, so
        # it is a foreign held key that blocks a start.
        g = self._guard(["ctrl_l"])
        g.note_press(_FakeModKey("ctrl_r"))
        self.assertTrue(g.foreign_key_held())
        self.assertFalse(g.may_start_on_target_down())

    def test_opposite_side_cancels(self):
        # Recording from ctrl_l; right Ctrl going down is a new foreign key.
        g = self._guard(["ctrl_l"])
        g.note_press(_FakeModKey("ctrl_l"))
        self.assertFalse(g.should_cancel_on_press(_FakeModKey("ctrl_l")))  # target
        self.assertTrue(g.should_cancel_on_press(_FakeModKey("ctrl_r")))   # foreign

    def test_generic_target_binding_treats_any_side_as_target(self):
        # A bound GENERIC ctrl: both sides and the generic are targets, none
        # foreign.
        g = self._guard(["ctrl"])
        for pressed in ("ctrl_l", "ctrl_r", "ctrl"):
            g.note_press(_FakeModKey(pressed))
        self.assertFalse(g.foreign_key_held())


class PynputQuitKeySideSpecificTests(unittest.TestCase):
    """The quit-key match is side-aware too (modifier_matches): a side-specific
    modifier quit key fires for its own side and the generic fallback, but NOT
    the opposite side."""

    def _ln(self, quit_key):
        guard = vp_keys_solo.SoloModifierGuard("<shift_r>", enabled=True)
        return vp_keys._PynputListener(
            _Target(), {"<shift_r>"}, quit_key, toggle_mode=False,
            solo_guard=guard)

    def test_same_side_quit_fires(self):
        ln = self._ln(_FakeModKey("ctrl_l"))   # quit bound to left Ctrl
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            ln.on_press(_FakeModKey("ctrl_l"))          # streak 1
            result = ln.on_press(_FakeModKey("ctrl_l"))  # streak 2 → quit
        self.assertFalse(result, "same-side modifier quit key must stop listener")

    def test_generic_variant_quit_fires(self):
        # Generic fallback also satisfies a side-specific quit key.
        ln = self._ln(_FakeModKey("ctrl_l"))
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            ln.on_press(_FakeModKey("ctrl"))
            result = ln.on_press(_FakeModKey("ctrl"))
        self.assertFalse(result)

    def test_opposite_side_does_not_quit(self):
        # THE reversal: right Ctrl must NOT trigger a left-Ctrl quit key. It
        # resets the streak instead, so the listener keeps running.
        ln = self._ln(_FakeModKey("ctrl_l"))
        with patch.object(vp_keys, "QUIT_COUNT", 2), \
                patch.object(vp_keys, "QUIT_WINDOW_MS", 10_000):
            r1 = ln.on_press(_FakeModKey("ctrl_r"))     # wrong side
            r2 = ln.on_press(_FakeModKey("ctrl_r"))     # wrong side again
        self.assertIsNone(r1)
        self.assertIsNone(r2, "opposite-side modifier must not trigger the quit")


class _SoloPynput:
    """Helper to drive a solo-guarded _PynputListener with simple str tokens."""

    @staticmethod
    def listener(target="<ctrl_l>", enabled=True):
        guard = vp_keys_solo.SoloModifierGuard(target, enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), {target}, "<esc>", toggle_mode=False, solo_guard=guard)


class PynputSoloModifierTests(unittest.TestCase):
    TARGET = "<ctrl_l>"

    def _ln(self, enabled=True):
        return _SoloPynput.listener(self.TARGET, enabled=enabled)

    def test_shift_then_ctrl_does_not_start(self):
        # (a) Reported bug: Shift held first, then the PTT modifier → no start.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("<shift_l>")     # foreign key first
            ln.on_press(self.TARGET)      # PTT modifier joins a chord
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_ctrl_alone_starts(self):
        # (b) part 1: PTT modifier pressed alone → start.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_chord_during_hold_cancels_and_discards(self):
        # (b) part 2: Ctrl alone → start; then C down → cancel + discard.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
            self.assertEqual(ln._owner.started, 1)
            ln.on_press("c")              # foreign key → Ctrl+C, cancel
            self.assertEqual(ln._owner.cancelled, 1)
            self.assertEqual(ln._owner.stopped, 0)  # no normal transcribe
            self.assertFalse(ln._recording)
            ln.on_release("c")
            ln.on_release(self.TARGET)    # release must NOT re-stop/transcribe
            self.assertEqual(ln._owner.stopped, 0)

    def test_solo_press_release_normal_transcribe(self):
        # (c) Ctrl alone → release → normal stop/transcribe path.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
            ln.on_release(self.TARGET)
        self.assertEqual(ln._owner.started, 1)
        self.assertEqual(ln._owner.stopped, 1)
        self.assertEqual(ln._owner.cancelled, 0)

    def test_key_repeat_of_ptt_key_does_not_cancel(self):
        # (e) Auto-repeat of the held PTT modifier must not be seen as a chord.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
            ln.on_press(self.TARGET)   # key-repeat
            ln.on_press(self.TARGET)   # key-repeat
        self.assertEqual(ln._owner.started, 1)
        self.assertEqual(ln._owner.cancelled, 0)
        self.assertTrue(ln._recording)

    def test_release_of_unseen_key_does_not_crash(self):
        # (f) Releasing a key never pressed (e.g. held before listener start).
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_release("<f7>")      # unknown release
            ln.on_press(self.TARGET)    # still starts cleanly afterwards
        self.assertEqual(ln._owner.started, 1)

    def test_media_key_during_hold_does_not_cancel(self):
        # (a) Real repro: PTT=ctrl_l recording, headset emits media_volume_up →
        # must NOT cancel; recording continues.
        media = _FakeMediaKey("media_volume_up")
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
            self.assertEqual(ln._owner.started, 1)
            ln.on_press(media)            # headset media key → ignored
            ln.on_release(media)
            self.assertEqual(ln._owner.cancelled, 0)
            self.assertTrue(ln._recording)
            ln.on_release(self.TARGET)    # normal stop/transcribe
            self.assertEqual(ln._owner.stopped, 1)

    def test_media_key_held_does_not_block_start(self):
        # (b) Headset media event arrives, then PTT modifier pressed → still
        # starts (media key must not be treated as a foreign held key).
        media = _FakeMediaKey("media_play_pause")
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(media)            # media key first
            ln.on_press(self.TARGET)      # PTT modifier alone → start
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_genuine_foreign_letter_still_cancels(self):
        # (c) Regression guard: a real foreign key (letter) STILL cancels.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.TARGET)
            ln.on_press("c")              # genuine foreign key → cancel
            self.assertEqual(ln._owner.cancelled, 1)
            self.assertFalse(ln._recording)


class PynputSoloModifierChordTests(unittest.TestCase):
    """Chord extension: an all-bare-modifier CHORD (shift+ctrl) is press-alone
    guarded just like a solo modifier — it must start only on that exact combo."""

    SHIFT = "<shift_l>"
    CTRL = "<ctrl_l>"

    def _ln(self, enabled=True):
        targets = {self.SHIFT, self.CTRL}
        guard = vp_keys_solo.SoloModifierGuard(targets, enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), set(targets), "<esc>", toggle_mode=False, solo_guard=guard)

    def test_shift_then_ctrl_chord_starts(self):
        # (a) Press shift, then ctrl (both targets) → starts.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.SHIFT)       # partial chord: nothing yet
            self.assertEqual(ln._owner.started, 0)
            ln.on_press(self.CTRL)        # chord complete (no foreign held) → start
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_foreign_then_chord_does_not_start(self):
        # (b) Hold X, then press shift+ctrl → no start (foreign key present).
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("x")              # foreign key held first
            ln.on_press(self.SHIFT)
            ln.on_press(self.CTRL)        # chord complete but X is held → blocked
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_foreign_key_during_chord_hold_cancels(self):
        # (c) Recording on shift+ctrl, press C during hold → cancel + discard.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.SHIFT)
            ln.on_press(self.CTRL)        # chord complete → start
            self.assertEqual(ln._owner.started, 1)
            ln.on_press("c")              # foreign key joins → cancel + discard
            self.assertEqual(ln._owner.cancelled, 1)
            self.assertEqual(ln._owner.stopped, 0)
            self.assertFalse(ln._recording)

    def test_release_one_chord_key_is_normal_stop(self):
        # (d) Recording on shift+ctrl, release ONE chord key → normal stop.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.SHIFT)
            ln.on_press(self.CTRL)        # start
            self.assertEqual(ln._owner.started, 1)
            ln.on_release(self.CTRL)      # one chord key up → normal stop/transcribe
        self.assertEqual(ln._owner.stopped, 1)
        self.assertEqual(ln._owner.cancelled, 0)
        self.assertFalse(ln._recording)

    def test_phantom_foreign_key_expiry_allows_chord_start(self):
        # (f) A phantom (never-released) foreign key self-heals past the expiry,
        # then the shift+ctrl chord can start.
        clock = [1000.0]
        targets = {self.SHIFT, self.CTRL}
        guard = vp_keys_solo.SoloModifierGuard(
            targets, enabled=True, _now=lambda: clock[0])
        ln = vp_keys._PynputListener(
            _Target(), set(targets), "<esc>", toggle_mode=False, solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("x")              # phantom foreign key, never released
            clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S + 0.1  # expire it
            ln.on_press(self.SHIFT)
            ln.on_press(self.CTRL)        # chord completes, phantom pruned → start
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_ctrl_then_shift_reversed_order_starts(self):
        # Chord completion is order-independent: ctrl first, then shift.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.CTRL)
            self.assertEqual(ln._owner.started, 0)
            ln.on_press(self.SHIFT)
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_target_held_past_expiry_then_chord_completes(self):
        # A TARGET held longer than the expiry (user thinks with shift down)
        # must never block its own chord: targets are excluded from the
        # foreign-key check even if pruned from the held set.
        clock = [1000.0]
        targets = {self.SHIFT, self.CTRL}
        guard = vp_keys_solo.SoloModifierGuard(
            targets, enabled=True, _now=lambda: clock[0])
        ln = vp_keys._PynputListener(
            _Target(), set(targets), "<esc>", toggle_mode=False, solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(self.SHIFT)
            clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S + 5.0  # long think
            ln.on_press(self.CTRL)        # chord completes → must start
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)


class EvdevSoloModifierTests(unittest.TestCase):
    TARGET = 29  # KEY_LEFTCTRL-ish code (opaque to the guard)
    SHIFT = 42
    C = 46

    def setUp(self):
        self.ev = _fake_evdev()
        self.t = _Target()

    def _guard(self, enabled=True):
        return vp_keys_solo.SoloModifierGuard(self.TARGET, enabled=enabled)

    def _apply(self, code, value, pressed, rec, solo):
        return self.t._evdev_apply_event(
            _event(1, code, value), self.ev, {self.TARGET}, pressed, rec, solo=solo)

    def test_shift_then_ctrl_does_not_start(self):
        # (a) Shift down (foreign) → Ctrl down → no start.
        pressed, solo = set(), self._guard()
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, pressed, False, solo)
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, rec, solo)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 0)

    def test_ctrl_alone_starts(self):
        # (b) part 1.
        pressed, solo = set(), self._guard()
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_chord_during_hold_cancels_and_discards(self):
        # (b) part 2: Ctrl alone → C down → cancel + discard.
        pressed, solo = set(), self._guard()
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
            self.assertTrue(rec)
            rec = self._apply(self.C, self.ev.KeyEvent.key_down, pressed, rec, solo)
            self.assertFalse(rec)
            self.assertEqual(self.t.cancelled, 1)
            self.assertEqual(self.t.stopped, 0)
            # Releasing C then the modifier must not start a normal transcribe.
            rec = self._apply(self.C, self.ev.KeyEvent.key_up, pressed, rec, solo)
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_up, pressed, rec, solo)
            self.assertEqual(self.t.stopped, 0)

    def test_solo_press_release_normal_transcribe(self):
        # (c).
        pressed, solo = set(), self._guard()
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_up, pressed, rec, solo)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 1)
        self.assertEqual(self.t.stopped, 1)
        self.assertEqual(self.t.cancelled, 0)

    def test_non_modifier_key_with_shift_held_still_activates(self):
        # (d) Non-modifier PTT key (solo disabled) with Shift held → unchanged.
        pressed, solo = set(), self._guard(enabled=False)
        # Shift held first (foreign), then the PTT key — must still start.
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, pressed, False, solo)
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_key_repeat_of_ptt_key_does_not_cancel(self):
        # (e) value-2 autorepeat of the held PTT key never cancels.
        pressed, solo = set(), self._guard()
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_hold, pressed, rec, solo)
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_hold, pressed, rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)
        self.assertEqual(self.t.cancelled, 0)

    def test_release_of_unseen_key_does_not_crash(self):
        # (f) Release of a key never pressed.
        pressed, solo = set(), self._guard()
        rec = self._apply(self.C, self.ev.KeyEvent.key_up, pressed, False, solo)
        self.assertFalse(rec)
        rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    # evdev consumer-control media code (KEY_VOLUMEUP == 115 on Linux). The
    # predicate's code set is patched so the test never needs real evdev.
    VOLUP = 115

    def test_media_key_during_hold_does_not_cancel(self):
        # (a) Recording on ctrl, KEY_VOLUMEUP down (headset) → no cancel, still
        # recording; then ctrl up → normal transcribe.
        pressed, solo = set(), self._guard()
        with patch.object(vp_keys_solo, "_IGNORED_EVDEV_CODES",
                          frozenset({self.VOLUP})), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
            self.assertTrue(rec)
            rec = self._apply(self.VOLUP, self.ev.KeyEvent.key_down, pressed, rec, solo)
            self.assertTrue(rec)                       # not cancelled
            self.assertEqual(self.t.cancelled, 0)
            self.assertNotIn(self.VOLUP, pressed)
            rec = self._apply(self.VOLUP, self.ev.KeyEvent.key_up, pressed, rec, solo)
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_up, pressed, rec, solo)
            self.assertEqual(self.t.stopped, 1)

    def test_media_key_held_does_not_block_start(self):
        # (b) KEY_VOLUMEUP arrives first, then ctrl → still starts.
        pressed, solo = set(), self._guard()
        with patch.object(vp_keys_solo, "_IGNORED_EVDEV_CODES",
                          frozenset({self.VOLUP})):
            rec = self._apply(self.VOLUP, self.ev.KeyEvent.key_down, pressed, False, solo)
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_genuine_foreign_letter_still_cancels(self):
        # (c) Regression: a real foreign key (letter) STILL cancels on evdev.
        pressed, solo = set(), self._guard()
        with patch.object(vp_keys_solo, "_IGNORED_EVDEV_CODES",
                          frozenset({self.VOLUP})), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.TARGET, self.ev.KeyEvent.key_down, pressed, False, solo)
            rec = self._apply(self.C, self.ev.KeyEvent.key_down, pressed, rec, solo)
            self.assertFalse(rec)
            self.assertEqual(self.t.cancelled, 1)


class EvdevSoloModifierChordTests(unittest.TestCase):
    """Chord extension for the evdev backend: a shift+ctrl modifier chord is
    press-alone guarded (target SET); ctrl+f9 (mixed) stays unguarded."""

    SHIFT = 42
    CTRL = 29
    C = 46
    F9 = 67

    def setUp(self):
        self.ev = _fake_evdev()
        self.t = _Target()

    def _guard(self, targets, enabled=True, now=None):
        kw = {} if now is None else {"_now": now}
        return vp_keys_solo.SoloModifierGuard(set(targets), enabled=enabled, **kw)

    def _apply(self, code, value, target_codes, pressed, rec, solo):
        return self.t._evdev_apply_event(
            _event(1, code, value), self.ev, set(target_codes), pressed, rec,
            solo=solo)

    def test_shift_then_ctrl_chord_starts(self):
        # (a) shift+ctrl binding: shift down, then ctrl → starts.
        targets = {self.SHIFT, self.CTRL}
        pressed, solo = set(), self._guard(targets)
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)
        self.assertFalse(rec)            # partial chord
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_foreign_then_chord_does_not_start(self):
        # (b) Hold X, press shift+ctrl → no start.
        targets = {self.SHIFT, self.CTRL}
        pressed, solo = set(), self._guard(targets)
        rec = self._apply(self.C, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)   # foreign key held first
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 0)

    def test_foreign_key_during_chord_hold_cancels(self):
        # (c) Recording on shift+ctrl, press C during hold → cancel + discard.
        targets = {self.SHIFT, self.CTRL}
        pressed, solo = set(), self._guard(targets)
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets,
                              pressed, False, solo)
            rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets,
                              pressed, rec, solo)
            self.assertTrue(rec)
            rec = self._apply(self.C, self.ev.KeyEvent.key_down, targets, pressed,
                              rec, solo)
            self.assertFalse(rec)
            self.assertEqual(self.t.cancelled, 1)
            self.assertEqual(self.t.stopped, 0)

    def test_release_one_chord_key_is_normal_stop(self):
        # (d) Recording on shift+ctrl, release one chord key → normal stop.
        targets = {self.SHIFT, self.CTRL}
        pressed, solo = set(), self._guard(targets)
        with patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets,
                              pressed, False, solo)
            rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets,
                              pressed, rec, solo)
            self.assertTrue(rec)
            rec = self._apply(self.CTRL, self.ev.KeyEvent.key_up, targets, pressed,
                              rec, solo)   # one chord key up → normal stop
            self.assertFalse(rec)
            self.assertEqual(self.t.stopped, 1)
            self.assertEqual(self.t.cancelled, 0)

    def test_mixed_binding_with_foreign_held_still_starts(self):
        # (e) ctrl+f9 mixed binding (guard disabled) with a foreign key held →
        # still starts, unchanged behaviour.
        targets = {self.CTRL, self.F9}
        # is_bare_modifier_binding(["ctrl_l", "f9"]) is False → guard disabled.
        self.assertFalse(
            vp_keys_solo.is_bare_modifier_binding(["ctrl_l", "f9"]))
        pressed, solo = set(), self._guard(targets, enabled=False)
        # Foreign key held first — but a disabled guard ignores it entirely. The
        # evdev loop only tracks foreign keys when the guard is enabled, so the
        # chord completes and starts normally.
        rec = self._apply(self.C, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        rec = self._apply(self.F9, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_phantom_foreign_key_expiry_allows_chord_start(self):
        # (f) Phantom foreign key (never released) self-heals past the expiry,
        # then the shift+ctrl chord starts.
        clock = [1000.0]
        targets = {self.SHIFT, self.CTRL}
        pressed = set()
        solo = self._guard(targets, now=lambda: clock[0])
        self._apply(self.C, self.ev.KeyEvent.key_down, targets, pressed, False,
                    solo)               # phantom foreign key
        clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S + 0.1
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)    # completes, phantom pruned → start
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_ctrl_then_shift_reversed_order_starts(self):
        # Chord completion is order-independent: ctrl first, then shift.
        targets = {self.SHIFT, self.CTRL}
        pressed, solo = set(), self._guard(targets)
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)
        self.assertFalse(rec)            # partial chord
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_target_held_past_expiry_then_chord_completes(self):
        # A TARGET held past the expiry (user thinks with shift down) must not
        # block its own chord. On evdev the held target self-prunes (note_repeat
        # skips targets) — harmless, because the foreign check excludes targets.
        clock = [1000.0]
        targets = {self.SHIFT, self.CTRL}
        pressed = set()
        solo = self._guard(targets, now=lambda: clock[0])
        rec = self._apply(self.SHIFT, self.ev.KeyEvent.key_down, targets, pressed,
                          False, solo)
        clock[0] += vp_keys_solo.FOREIGN_KEY_EXPIRY_S + 5.0  # long think
        rec = self._apply(self.CTRL, self.ev.KeyEvent.key_down, targets, pressed,
                          rec, solo)     # completes → must start
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)


class EvdevSideSpecificTests(unittest.TestCase):
    """evdev is already unambiguously side-specific: KEY_LEFTCTRL and
    KEY_RIGHTCTRL are distinct integer scancodes and there is no generic variant,
    so a ctrl_l binding (resolved to KEY_LEFTCTRL) is matched by left Ctrl only.
    _EVDEV_MAP must resolve every side of every modifier family."""

    LEFTCTRL = 29
    RIGHTCTRL = 97

    def setUp(self):
        self.ev = _fake_evdev()
        self.t = _Target()

    def _apply(self, code, value, target_codes, pressed, rec, solo=None):
        return self.t._evdev_apply_event(
            _event(1, code, value), self.ev, set(target_codes), pressed, rec,
            solo=solo)

    def test_left_ctrl_matches_left_binding(self):
        # ctrl_l binding → {KEY_LEFTCTRL}; left Ctrl down starts.
        pressed = set()
        rec = self._apply(self.LEFTCTRL, self.ev.KeyEvent.key_down,
                          {self.LEFTCTRL}, pressed, False)
        self.assertTrue(rec)
        self.assertEqual(self.t.started, 1)

    def test_right_ctrl_does_not_match_left_binding(self):
        # THE reversal on evdev: right Ctrl is a different scancode → no start.
        pressed = set()
        rec = self._apply(self.RIGHTCTRL, self.ev.KeyEvent.key_down,
                          {self.LEFTCTRL}, pressed, False)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 0)

    def test_right_ctrl_is_foreign_under_solo_guard(self):
        # With the bare-modifier guard on a ctrl_l binding, right Ctrl is a
        # foreign held key: pressed first, it blocks the left-Ctrl start.
        pressed = set()
        solo = vp_keys_solo.SoloModifierGuard({self.LEFTCTRL}, enabled=True)
        rec = self._apply(self.RIGHTCTRL, self.ev.KeyEvent.key_down,
                          {self.LEFTCTRL}, pressed, False, solo)   # foreign first
        rec = self._apply(self.LEFTCTRL, self.ev.KeyEvent.key_down,
                          {self.LEFTCTRL}, pressed, rec, solo)
        self.assertFalse(rec)
        self.assertEqual(self.t.started, 0)

    def test_evdev_map_resolves_all_sides(self):
        # _EVDEV_MAP must resolve every side of every modifier family the capture
        # / settings can emit, so a side-specific binding never sys.exits.
        m = vp_keys.KeyBackendMixin._EVDEV_MAP
        expected = {
            "ctrl_l": "KEY_LEFTCTRL", "ctrl_r": "KEY_RIGHTCTRL",
            "shift_l": "KEY_LEFTSHIFT", "shift_r": "KEY_RIGHTSHIFT",
            "alt_l": "KEY_LEFTALT", "alt_r": "KEY_RIGHTALT",
            "cmd_l": "KEY_LEFTMETA", "cmd_r": "KEY_RIGHTMETA",
            "super_l": "KEY_LEFTMETA", "super_r": "KEY_RIGHTMETA",
        }
        for name, ec in expected.items():
            self.assertEqual(m.get(name), ec, name)

    def test_evdev_target_codes_resolves_each_side_distinctly(self):
        import types as _local_types
        evdev = _local_types.ModuleType("evdev")
        evdev.ecodes = types.SimpleNamespace(
            KEY_LEFTCTRL=29, KEY_RIGHTCTRL=97)
        left = vp_keys.KeyBackendMixin._evdev_target_codes(
            self.t, evdev, ["ctrl_l"])
        right = vp_keys.KeyBackendMixin._evdev_target_codes(
            self.t, evdev, ["ctrl_r"])
        self.assertEqual(left, {29})
        self.assertEqual(right, {97})
        self.assertNotEqual(left, right, "the two sides must be distinct codes")


class ModifierMatchesUnitTests(unittest.TestCase):
    """``modifier_matches`` is the single side-aware matching predicate that
    reverses #254's full side-insensitivity: a side-specific target matches its
    OWN side and the generic family fallback, but NOT the opposite side; a
    generic target matches any side."""

    mm = staticmethod(vp_keys_solo.modifier_matches)

    # --- side-specific target: ctrl_l ------------------------------------------
    def test_side_specific_target_matches_same_side(self):
        self.assertTrue(self.mm(_FakeModKey("ctrl_l"), "ctrl_l"))

    def test_side_specific_target_matches_generic_fallback(self):
        # The reliability fail-safe: a generic Key.ctrl (side unknown) still
        # satisfies a ctrl_l binding so the chord starts.
        self.assertTrue(self.mm(_FakeModKey("ctrl"), "ctrl_l"))

    def test_side_specific_target_rejects_opposite_side(self):
        # THE reversal: right Ctrl must NOT satisfy a left-Ctrl binding.
        self.assertFalse(self.mm(_FakeModKey("ctrl_r"), "ctrl_l"))

    def test_alt_gr_and_alt_r_are_equivalent(self):
        # AltGr is physically the right Alt: a press reported as one spelling must
        # satisfy a binding saved as the other (backward-compat for AltGr users).
        self.assertTrue(self.mm(_FakeModKey("alt_gr"), "alt_r"))
        self.assertTrue(self.mm(_FakeModKey("alt_r"), "alt_gr"))
        # ...but LEFT alt must NOT satisfy a right-alt (alt_r/alt_gr) binding.
        self.assertFalse(self.mm(_FakeModKey("alt_l"), "alt_r"))
        self.assertFalse(self.mm(_FakeModKey("alt_l"), "alt_gr"))

    def test_ctrl_r_target_is_symmetric(self):
        self.assertTrue(self.mm(_FakeModKey("ctrl_r"), "ctrl_r"))
        self.assertTrue(self.mm(_FakeModKey("ctrl"), "ctrl_r"))   # generic fallback
        self.assertFalse(self.mm(_FakeModKey("ctrl_l"), "ctrl_r"))

    # --- generic target: bare ctrl (matches any side) --------------------------
    def test_generic_target_matches_every_variant(self):
        for pressed in ("ctrl_l", "ctrl_r", "ctrl"):
            self.assertTrue(self.mm(_FakeModKey(pressed), "ctrl"),
                            f"{pressed} should match generic ctrl target")

    # --- different family never matches ----------------------------------------
    def test_different_family_never_matches(self):
        self.assertFalse(self.mm(_FakeModKey("shift_l"), "ctrl_l"))
        self.assertFalse(self.mm(_FakeModKey("alt"), "ctrl"))

    # --- non-modifier targets: plain name equality, unchanged ------------------
    def test_non_modifier_target_exact_match(self):
        self.assertTrue(self.mm(_FakeModKey("f9"), "f9"))
        self.assertFalse(self.mm(_FakeModKey("f9"), "f12"))
        # The string-token form the other tests use.
        self.assertTrue(self.mm("<ctrl_r>", "<ctrl_r>"))
        self.assertFalse(self.mm("<ctrl_r>", "<ctrl_l>"))
        # A single-char quit key.
        self.assertTrue(self.mm("q", "q"))

    def test_single_char_keycode_target_matches_via_char(self):
        # #274 regression: a quit key configured as "q" arrives as a KeyCode
        # (.char="q", no .name). It must match the saved "q" target and not a
        # different char.
        self.assertTrue(self.mm(_FakeCharKey("q"), "q"))
        self.assertFalse(self.mm(_FakeCharKey("x"), "q"))

    def test_unnameable_or_bad_target_never_matches(self):
        self.assertFalse(self.mm(_FakeMediaKey("media_volume_up"), "ctrl_l"))
        self.assertFalse(self.mm(object(), "ctrl_l"))
        self.assertFalse(self.mm(_FakeModKey("ctrl_l"), None))

    def test_alt_gr_is_alt_family_side_specific(self):
        # alt_gr is its own resolvable name in the alt family. It matches a
        # generic alt target (family) and an alt_gr target, but a side-specific
        # alt_l target is only satisfied by alt_l or the generic alt fallback —
        # alt_gr (a distinct specific variant) does not satisfy alt_l.
        self.assertTrue(self.mm(_FakeModKey("alt_gr"), "alt"))
        self.assertTrue(self.mm(_FakeModKey("alt_gr"), "alt_gr"))
        self.assertFalse(self.mm(_FakeModKey("alt_gr"), "alt_l"))


class ModifierHelpersUnitTests(unittest.TestCase):
    """The surviving family helpers used by capture's generic-fallback and by
    the pynput release matching (``canon_modifier`` / ``modifier_family`` /
    ``key_name``). ``canon_modifier`` is no longer used for PTT matching."""

    def test_canon_modifier_collapses_families(self):
        for fam in ("ctrl", "shift", "alt", "cmd"):
            left = vp_keys_solo.canon_modifier(_FakeModKey(f"{fam}_l"))
            right = vp_keys_solo.canon_modifier(_FakeModKey(f"{fam}_r"))
            generic = vp_keys_solo.canon_modifier(_FakeModKey(fam))
            self.assertEqual(left, fam)
            self.assertEqual(right, fam)
            self.assertEqual(generic, fam)
        self.assertEqual(vp_keys_solo.canon_modifier(_FakeModKey("alt_gr")), "alt")

    def test_canon_modifier_passes_non_modifiers_through(self):
        for tok in ("c", "<ctrl_l>", "<esc>", _FakeModKey("f9"),
                    _FakeMediaKey("media_volume_up")):
            self.assertIs(vp_keys_solo.canon_modifier(tok), tok)

    def test_modifier_family(self):
        self.assertEqual(vp_keys_solo.modifier_family(_FakeModKey("ctrl_l")), "ctrl")
        self.assertEqual(vp_keys_solo.modifier_family(_FakeModKey("ctrl")), "ctrl")
        self.assertEqual(vp_keys_solo.modifier_family("shift_r"), "shift")
        self.assertIsNone(vp_keys_solo.modifier_family(_FakeModKey("f9")))
        self.assertIsNone(vp_keys_solo.modifier_family(object()))

    def test_key_name(self):
        self.assertEqual(vp_keys_solo.key_name(_FakeModKey("ctrl_l")), "ctrl_l")
        self.assertEqual(vp_keys_solo.key_name("ctrl_r"), "ctrl_r")
        self.assertIsNone(vp_keys_solo.key_name(object()))

    def test_key_name_extracts_char_from_keycode(self):
        # #274 regression: a single-char KeyCode (e.g. quit key "q") has .char
        # but no .name; key_name must return the char so the quit match (via
        # modifier_matches) works. A charless token still yields None.
        self.assertEqual(vp_keys_solo.key_name(_FakeCharKey("q")), "q")
        self.assertIsNone(vp_keys_solo.key_name(_FakeCharKey(None)))


class HeldKeysClearedByReleaseUnitTests(unittest.TestCase):
    """``held_keys_cleared_by_release`` is the SINGLE source of truth for
    side-aware release clearing, shared by ``SoloModifierGuard.note_release`` AND
    the pynput listener's ``_discard_held`` so both release paths agree. A generic
    release clears the whole family (side unknown → fail-safe); a side-specific
    release clears only the same side (``alt_gr``≡``alt_r``) plus any held generic,
    LEAVING the opposite side held; a non-modifier release drops only the exact
    token."""

    cleared = staticmethod(vp_keys_solo.held_keys_cleared_by_release)

    def test_generic_release_clears_whole_family(self):
        left, right, gen = (_FakeModKey("ctrl_l"), _FakeModKey("ctrl_r"),
                            _FakeModKey("ctrl"))
        dropped = self.cleared([left, right, gen], _FakeModKey("ctrl"))
        self.assertCountEqual(dropped, [left, right, gen])

    def test_side_specific_release_leaves_opposite_side_held(self):
        left, right = _FakeModKey("ctrl_l"), _FakeModKey("ctrl_r")
        dropped = self.cleared([left, right], _FakeModKey("ctrl_l"))
        self.assertEqual(dropped, [left])        # only the same side
        self.assertNotIn(right, dropped)         # opposite stays held

    def test_side_specific_release_also_clears_held_generic(self):
        left, gen = _FakeModKey("ctrl_l"), _FakeModKey("ctrl")
        dropped = self.cleared([left, gen], _FakeModKey("ctrl_l"))
        self.assertCountEqual(dropped, [left, gen])  # same side + held generic

    def test_alt_r_release_clears_held_alt_gr_and_vice_versa(self):
        alt_gr = _FakeModKey("alt_gr")
        self.assertEqual(self.cleared([alt_gr], _FakeModKey("alt_r")), [alt_gr])
        alt_r = _FakeModKey("alt_r")
        self.assertEqual(self.cleared([alt_r], _FakeModKey("alt_gr")), [alt_r])
        # ...but the LEFT alt must survive a right-alt / alt_gr release.
        alt_l = _FakeModKey("alt_l")
        self.assertEqual(self.cleared([alt_l], _FakeModKey("alt_gr")), [])

    def test_release_does_not_touch_other_family(self):
        ctrl, shift = _FakeModKey("ctrl_l"), _FakeModKey("shift_l")
        dropped = self.cleared([ctrl, shift], _FakeModKey("ctrl"))
        self.assertEqual(dropped, [ctrl])
        self.assertNotIn(shift, dropped)

    def test_non_modifier_release_exact_only(self):
        # A released letter drops only that exact token, never a held modifier.
        ctrl = _FakeModKey("ctrl_l")
        held = ["a", ctrl]
        self.assertEqual(self.cleared(held, "a"), ["a"])
        # An unknown released token not present in held → nothing dropped.
        self.assertEqual(self.cleared(held, "z"), [])


class AllTargetsHaveDistinctMatchUnitTests(unittest.TestCase):
    """``all_targets_have_distinct_match`` requires a 1:1 (injective) assignment
    of held keys to target names, so the generic fallback cannot let ONE held
    ``Key.ctrl`` satisfy both ``ctrl_l`` and ``ctrl_r`` in a both-sides binding
    (#274 Copilot finding)."""

    f = staticmethod(vp_keys_solo.all_targets_have_distinct_match)

    def test_single_target(self):
        self.assertTrue(self.f(["ctrl_l"], [_FakeModKey("ctrl_l")]))
        self.assertTrue(self.f(["ctrl_l"], [_FakeModKey("ctrl")]))    # generic fallback
        self.assertFalse(self.f(["ctrl_l"], [_FakeModKey("ctrl_r")]))  # opposite side

    def test_both_sides_binding_needs_two_distinct_held(self):
        names = ["ctrl_l", "ctrl_r"]
        # One generic Ctrl matches BOTH names but is a SINGLE key → not complete.
        self.assertFalse(self.f(names, [_FakeModKey("ctrl")]))
        # Two distinct specific sides → complete.
        self.assertTrue(self.f(names, [_FakeModKey("ctrl_l"), _FakeModKey("ctrl_r")]))
        # One specific + one generic (generic covers the other side) → complete.
        self.assertTrue(self.f(names, [_FakeModKey("ctrl_l"), _FakeModKey("ctrl")]))

    def test_two_family_chord(self):
        names = ["ctrl_l", "shift_l"]
        self.assertTrue(self.f(names, [_FakeModKey("ctrl"), _FakeModKey("shift")]))
        # Two held keys but the second matches no remaining target → incomplete.
        self.assertFalse(self.f(names, [_FakeModKey("ctrl"), _FakeModKey("alt")]))

    def test_fewer_held_than_targets_short_circuits(self):
        self.assertFalse(self.f(["ctrl_l", "ctrl_r"], [_FakeModKey("ctrl_l")]))


class PynputSideSpecificChordTests(unittest.TestCase):
    """Side-specific chord matching (reverses #254). A ``shift_l+ctrl_l`` binding
    completes for the BOUND sides (and the generic-family fallback) but NOT for
    the opposite side. Targets are REAL-ish Key objects (``_FakeModKey`` exposing
    ``.name``) so the full side-aware path (modifier_matches) is exercised end to
    end, including the solo guard (bare-modifier chord → enabled)."""

    def _ln(self, enabled=True):
        shift_l = _FakeModKey("shift_l")
        ctrl_l = _FakeModKey("ctrl_l")
        targets = {shift_l, ctrl_l}
        guard = vp_keys_solo.SoloModifierGuard(set(targets), enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), set(targets), _FakeModKey("esc"), toggle_mode=False,
            solo_guard=guard)

    def test_shift_then_ctrl_left_keys_start(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            self.assertEqual(ln._owner.started, 0)   # partial chord
            ln.on_press(_FakeModKey("ctrl_l"))
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_ctrl_then_shift_left_keys_start(self):
        # Order-independent: same chord, reversed press order.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
            self.assertEqual(ln._owner.started, 0)
            ln.on_press(_FakeModKey("shift_l"))
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_generic_ctrl_variant_completes_chord(self):
        # Reliability fail-safe: ctrl_l is physically pressed but pynput delivers
        # the GENERIC Key.ctrl (side unknown). The side-specific ctrl_l target is
        # still satisfied by the generic fallback, so the chord completes.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl"))      # generic variant for ctrl_l
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_generic_variant_reversed_order_also_starts(self):
        # Generic fallback works in either press order.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
            ln.on_press(_FakeModKey("shift"))     # generic shift last
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_opposite_side_member_does_not_complete_chord(self):
        # THE reversal: the bound left ctrl is NOT satisfied by right ctrl, so the
        # chord stays incomplete and recording never starts.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl_r"))    # WRONG side for ctrl_l
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_opposite_side_member_during_hold_cancels(self):
        # Recording on shift_l+ctrl_l, then the wrong-side ctrl_r goes down: it is
        # NOT a chord member (different side) → counts as foreign → cancel.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl_l"))
            self.assertEqual(ln._owner.started, 1)
            ln.on_press(_FakeModKey("ctrl_r"))    # opposite side → foreign
        self.assertEqual(ln._owner.cancelled, 1)
        self.assertFalse(ln._recording)

    def test_release_of_different_variant_stops(self):
        # press ctrl_l, release reported as generic ctrl (same family): the chord
        # must still break and stop (no stuck recording).
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl_l"))
            self.assertEqual(ln._owner.started, 1)
            ln.on_release(_FakeModKey("ctrl"))    # generic up for the left key
        self.assertEqual(ln._owner.stopped, 1)
        self.assertFalse(ln._recording)

    def test_generic_member_repeat_is_not_foreign(self):
        # Solo-guard: a generic variant of a held chord member must NOT be treated
        # as a foreign key (no cancel). Hold the chord, then a repeat arrives as
        # the generic family variant → still recording.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl_l"))
            self.assertEqual(ln._owner.started, 1)
            ln.on_press(_FakeModKey("ctrl"))      # generic repeat of ctrl_l member
        self.assertEqual(ln._owner.cancelled, 0)
        self.assertTrue(ln._recording)

    def test_genuine_foreign_letter_still_cancels(self):
        # The solo guard must still cancel on a real foreign key during the hold.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl_l"))
            ln.on_press("x")                      # genuine foreign key → cancel
        self.assertEqual(ln._owner.cancelled, 1)
        self.assertFalse(ln._recording)

    def test_foreign_then_chord_does_not_start(self):
        # Rule 1 still holds for a real foreign key: hold X, then complete the
        # chord (with a generic variant) → blocked.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press("x")                      # foreign held first
            ln.on_press(_FakeModKey("shift_l"))
            ln.on_press(_FakeModKey("ctrl"))      # chord complete but X held
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_discard_held_side_specific_leaves_opposite_side(self):
        # The listener-side equivalent of note_release's over-deletion fix: with
        # BOTH ctrl sides physically held, a side-specific release of ctrl_l must
        # drop only ctrl_l from _held_keys and LEAVE ctrl_r held — otherwise the
        # still-pressed opposite side would be wrongly forgotten and a held
        # foreign key could be lost. Shared via held_keys_cleared_by_release.
        ln = self._ln()
        ctrl_l, ctrl_r = _FakeModKey("ctrl_l"), _FakeModKey("ctrl_r")
        ln._held_keys = {ctrl_l, ctrl_r}
        ln._discard_held(ctrl_l)
        self.assertIn(ctrl_r, ln._held_keys)
        self.assertNotIn(ctrl_l, ln._held_keys)

    def test_discard_held_generic_release_clears_family(self):
        # A generic ctrl up (side unknown) clears every held ctrl variant so the
        # chord can always break — the fail-safe direction.
        ln = self._ln()
        ctrl_l, ctrl_r = _FakeModKey("ctrl_l"), _FakeModKey("ctrl_r")
        ln._held_keys = {ctrl_l, ctrl_r}
        ln._discard_held(_FakeModKey("ctrl"))
        self.assertEqual(ln._held_keys, set())

    def test_both_sides_binding_not_completed_by_one_generic(self):
        # ctrl_l+ctrl_r binding: a single generic Key.ctrl matches BOTH targets
        # via the fallback, but must NOT complete the chord on its own — two
        # distinct held keys are required (Copilot #274 finding). A second,
        # distinct token then completes it.
        ctrl_l, ctrl_r = _FakeModKey("ctrl_l"), _FakeModKey("ctrl_r")
        guard = vp_keys_solo.SoloModifierGuard({ctrl_l, ctrl_r}, enabled=True)
        ln = vp_keys._PynputListener(
            _Target(), {ctrl_l, ctrl_r}, _FakeModKey("esc"),
            toggle_mode=False, solo_guard=guard)
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl"))        # one generic Ctrl
            self.assertEqual(ln._owner.started, 0)  # NOT complete on one key
            ln.on_press(_FakeModKey("ctrl_r"))      # a second, distinct token
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)


class PynputSideSpecificSingleKeyTests(unittest.TestCase):
    """Single-key side-specific binding: a ctrl_l binding is started by left Ctrl
    and the generic Key.ctrl fallback, but NOT by right Ctrl."""

    def _ln(self, enabled=True):
        target = _FakeModKey("ctrl_l")
        guard = vp_keys_solo.SoloModifierGuard({target}, enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), {target}, _FakeModKey("esc"), toggle_mode=False,
            solo_guard=guard)

    def test_same_side_starts(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_generic_variant_starts_single_key_binding(self):
        # Generic Key.ctrl (side unknown) is the fail-safe fallback for ctrl_l.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl"))      # generic for the ctrl_l binding
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)

    def test_opposite_side_does_not_start(self):
        # THE reversal at the single-key level: right Ctrl must not start a
        # left-Ctrl binding.
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_r"))    # WRONG side
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)

    def test_press_variant_release_other_variant_stops(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
            ln.on_release(_FakeModKey("ctrl"))    # generic (same family) up
        self.assertEqual(ln._owner.started, 1)
        self.assertEqual(ln._owner.stopped, 1)
        self.assertFalse(ln._recording)

    def test_variant_repeat_does_not_restart(self):
        # Hold mode: the same physical key repeating as same-side/generic variants
        # must start exactly once (rising-edge latch over the held set).
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
            ln.on_press(_FakeModKey("ctrl"))      # repeat as generic
            ln.on_press(_FakeModKey("ctrl_l"))    # repeat as left
        self.assertEqual(ln._owner.started, 1)
        self.assertTrue(ln._recording)


class PynputSideSpecificGenericBindingTests(unittest.TestCase):
    """A GENERIC (sideless) binding of bare ``ctrl`` keeps matching ANY side —
    side-specific matching only narrows side-SPECIFIC bindings."""

    def _ln(self, enabled=True):
        target = _FakeModKey("ctrl")        # generic, sideless binding
        guard = vp_keys_solo.SoloModifierGuard({target}, enabled=enabled)
        return vp_keys._PynputListener(
            _Target(), {target}, _FakeModKey("esc"), toggle_mode=False,
            solo_guard=guard)

    def test_left_side_starts_generic_binding(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_l"))
        self.assertEqual(ln._owner.started, 1)

    def test_right_side_starts_generic_binding(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_r"))
        self.assertEqual(ln._owner.started, 1)

    def test_generic_press_starts_generic_binding(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl"))
        self.assertEqual(ln._owner.started, 1)


class PynputSideSpecificToggleTests(unittest.TestCase):
    """toggle_mode is side-specific too: a same-side or generic press starts, the
    opposite side does not, and a second press (same side/generic) stops."""

    def _ln(self):
        target = _FakeModKey("ctrl_l")
        guard = vp_keys_solo.SoloModifierGuard({target}, enabled=True)
        return vp_keys._PynputListener(
            _Target(), {target}, _FakeModKey("esc"), toggle_mode=True,
            solo_guard=guard)

    def test_toggle_with_same_and_generic_variants(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl"))      # generic press (fallback): start
            self.assertEqual(ln._owner.started, 1)
            self.assertTrue(ln._recording)
            ln.on_release(_FakeModKey("ctrl_l"))  # release ignored in toggle mode
            self.assertEqual(ln._owner.stopped, 0)
            self.assertTrue(ln._recording)
            ln.on_press(_FakeModKey("ctrl_l"))    # second press (left): stop
        self.assertEqual(ln._owner.stopped, 1)
        self.assertFalse(ln._recording)

    def test_toggle_opposite_side_does_not_start(self):
        ln = self._ln()
        with patch.object(vp_keys, "QUIT_COUNT", 0), \
                patch.object(vp_keys.threading, "Thread", _ImmediateThread):
            ln.on_press(_FakeModKey("ctrl_r"))    # WRONG side
        self.assertEqual(ln._owner.started, 0)
        self.assertFalse(ln._recording)


if __name__ == "__main__":
    unittest.main()
