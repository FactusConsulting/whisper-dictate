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

    def test_non_target_key_press_does_not_accumulate_in_pressed(self):
        # Comment 1 fix: non-target keys must never be added to _pressed, so the
        # set stays bounded (only target keys tracked for chord completion).
        ln = self._listener()
        with patch.object(vp_keys, "QUIT_COUNT", 0):
            ln.on_press("<shift_l>")   # foreign key — not a target
            ln.on_release("<shift_l>")
        self.assertNotIn("<shift_l>", ln._pressed)
        self.assertEqual(len(ln._pressed), 0)

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



class SoloModifierGuardUnitTests(unittest.TestCase):
    """Backend-agnostic state machine in vp_keys_solo."""

    def test_is_bare_modifier_key(self):
        self.assertTrue(vp_keys_solo.is_bare_modifier_key(["ctrl_l"]))
        self.assertTrue(vp_keys_solo.is_bare_modifier_key(["shift_r"]))
        self.assertTrue(vp_keys_solo.is_bare_modifier_key(["super_l"]))
        # Non-modifier single key and multi-key chords are NOT guarded.
        self.assertFalse(vp_keys_solo.is_bare_modifier_key(["scroll_lock"]))
        self.assertFalse(vp_keys_solo.is_bare_modifier_key(["f9"]))
        self.assertFalse(vp_keys_solo.is_bare_modifier_key(["shift_r", "ctrl_r"]))

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


if __name__ == "__main__":
    unittest.main()
