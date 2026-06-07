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


if __name__ == "__main__":
    unittest.main()
