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

    def _start(self):
        self.started += 1

    def _stop_and_transcribe(self):
        self.stopped += 1


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


if __name__ == "__main__":
    unittest.main()
