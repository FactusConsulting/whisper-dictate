"""Unit tests for the press-to-capture hotkey binding (#258 part 1).

The pure chord-assembly logic (vp_keys_capture.ChordCapture +
key_to_setting_name) is exercised here by feeding synthetic press/release
sequences — no real keyboard, no pynput listener. The live listener
(vp_keys_capture_io) is the thin IO shell and is not unit-tested (it only wires
these pure pieces to a global hook).

Key tokens are modelled with the same hashable, ``.name``-bearing fakes the
existing test_keys.py uses for pynput ``Key`` enum members, plus a ``.char``
fake for letter ``KeyCode``s.
"""
from helpers import unittest

from whisper_dictate import vp_keys_capture
from whisper_dictate.vp_keys_capture import (
    ChordCapture,
    canonical_chord,
    key_to_setting_name,
)


class _Key:
    """Hashable stand-in for a pynput ``Key`` enum member (exposes ``.name``).

    Two with the same name are equal (the OS may emit ``Key.ctrl`` twice);
    different names are distinct (``ctrl_l`` vs ``ctrl`` vs ``ctrl_r``), exactly
    like the real enum — which is what side-insensitive canonicalisation papers
    over."""

    def __init__(self, name):
        self.name = name

    def __hash__(self):
        return hash(self.name)

    def __eq__(self, other):
        return isinstance(other, _Key) and other.name == self.name

    def __repr__(self):
        return f"<Key.{self.name}>"


class _Char:
    """Hashable stand-in for a pynput letter ``KeyCode`` (exposes ``.char``)."""

    def __init__(self, char):
        self.char = char

    def __hash__(self):
        return hash(self.char)

    def __eq__(self, other):
        return isinstance(other, _Char) and other.char == self.char

    def __repr__(self):
        return f"<KeyCode {self.char}>"


class _Unbindable:
    """A token with neither a usable ``.name`` nor ``.char`` (raw VK KeyCode)."""

    def __init__(self, tag):
        self._tag = tag

    def __hash__(self):
        return hash(self._tag)

    def __eq__(self, other):
        return isinstance(other, _Unbindable) and other._tag == self._tag


class KeyToSettingNameTests(unittest.TestCase):
    def test_modifier_collapses_side_insensitively(self):
        # Left and right Ctrl both bind to the same concrete name.
        self.assertEqual(key_to_setting_name(_Key("ctrl_l")), "ctrl_r")
        self.assertEqual(key_to_setting_name(_Key("ctrl_r")), "ctrl_r")
        # The generic variant pynput sometimes delivers collapses too.
        self.assertEqual(key_to_setting_name(_Key("ctrl")), "ctrl_r")

    def test_each_modifier_family_maps_to_concrete_name(self):
        self.assertEqual(key_to_setting_name(_Key("shift_l")), "shift_r")
        self.assertEqual(key_to_setting_name(_Key("alt_l")), "alt_r")
        self.assertEqual(key_to_setting_name(_Key("alt_gr")), "alt_r")  # via canon
        self.assertEqual(key_to_setting_name(_Key("cmd_l")), "cmd_r")

    def test_named_special_key_passthrough(self):
        self.assertEqual(key_to_setting_name(_Key("f9")), "f9")
        self.assertEqual(key_to_setting_name(_Key("space")), "space")

    def test_letter_keycode_uses_char(self):
        self.assertEqual(key_to_setting_name(_Char("a")), "a")

    def test_unbindable_token_returns_none(self):
        self.assertIsNone(key_to_setting_name(_Unbindable("vk-1234")))


class CanonicalChordTests(unittest.TestCase):
    def test_sorted_and_deduped(self):
        self.assertEqual(canonical_chord(["shift_r", "ctrl_r"]), "ctrl_r+shift_r")
        self.assertEqual(canonical_chord(["ctrl_r", "ctrl_r"]), "ctrl_r")

    def test_empty(self):
        self.assertEqual(canonical_chord([]), "")


class ChordCaptureSingleKeyTests(unittest.TestCase):
    def test_single_tap_binds_that_key(self):
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        self.assertIsNone(c.release(_Key("alt_r")))  # unrelated release: ignored?
        # ^ alt_r was never pressed → discard does nothing; ctrl_r still held.
        self.assertFalse(c.done)
        result = c.release(_Key("ctrl_r"))
        self.assertEqual(result, "ctrl_r")
        self.assertTrue(c.done)
        self.assertEqual(c.result, "ctrl_r")

    def test_letter_single_key(self):
        c = ChordCapture()
        c.press(_Char("a"))
        self.assertEqual(c.release(_Char("a")), "a")


class ChordCaptureMultiKeyTests(unittest.TestCase):
    def test_two_key_chord_release_one_then_the_other(self):
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("shift_r"))
        # Release one: chord not complete yet, still holding the other.
        self.assertIsNone(c.release(_Key("ctrl_r")))
        self.assertFalse(c.done)
        # Release the last: emit the FULL chord built up, not just the last key.
        self.assertEqual(c.release(_Key("shift_r")), "ctrl_r+shift_r")

    def test_release_captures_full_set_high_water_mark(self):
        # Build a 3-key chord, then let go one at a time — the captured chord is
        # the high-water mark (all three), never eroded by partial releases.
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("shift_r"))
        c.press(_Key("alt_r"))
        self.assertEqual(sorted(c.held_names()), ["alt_r", "ctrl_r", "shift_r"])
        self.assertIsNone(c.release(_Key("alt_r")))
        self.assertIsNone(c.release(_Key("ctrl_r")))
        self.assertEqual(c.release(_Key("shift_r")), "alt_r+ctrl_r+shift_r")

    def test_press_order_does_not_change_canonical_chord(self):
        a = ChordCapture()
        a.press(_Key("shift_r"))
        a.press(_Key("ctrl_r"))
        chord_a = a.release(_Key("shift_r")) or a.release(_Key("ctrl_r"))
        b = ChordCapture()
        b.press(_Key("ctrl_r"))
        b.press(_Key("shift_r"))
        chord_b = b.release(_Key("ctrl_r")) or b.release(_Key("shift_r"))
        self.assertEqual(chord_a, chord_b, "chord is order-independent")
        self.assertEqual(chord_a, "ctrl_r+shift_r")

    def test_left_and_right_same_family_dedupe_to_one(self):
        # Holding BOTH left and right Ctrl must bind a single ctrl_r, not a
        # two-member chord — side-insensitive canonicalisation collapses them.
        c = ChordCapture()
        c.press(_Key("ctrl_l"))
        c.press(_Key("ctrl_r"))
        self.assertIsNone(c.release(_Key("ctrl_l")))
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_r")


class ChordCaptureEdgeCaseTests(unittest.TestCase):
    def test_key_repeat_is_ignored(self):
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("ctrl_r"))  # OS key-repeat
        c.press(_Key("ctrl_r"))
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_r")

    def test_release_of_never_pressed_key_is_ignored(self):
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        self.assertIsNone(c.release(_Key("alt_r")))  # never pressed
        self.assertFalse(c.done)
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_r")

    def test_inert_after_capture(self):
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_r")
        # Further events do nothing; result is sticky.
        c.press(_Key("shift_r"))
        self.assertIsNone(c.release(_Key("shift_r")))
        self.assertEqual(c.result, "ctrl_r")

    def test_unbindable_key_does_not_wedge_capture(self):
        # A token we cannot bind never enters the held set, so a real chord still
        # finalises on release (it must not keep _held non-empty forever).
        c = ChordCapture()
        c.press(_Unbindable("vk-99"))
        c.press(_Key("f9"))
        self.assertEqual(c.release(_Key("f9")), "f9")

    def test_release_with_nothing_captured_returns_none(self):
        c = ChordCapture()
        self.assertIsNone(c.release(_Key("ctrl_r")))
        self.assertFalse(c.done)


class ChordCaptureMediaTests(unittest.TestCase):
    def test_media_key_dropped_by_default(self):
        # A stray Bluetooth volume event must not pollute a keyboard chord.
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("media_volume_up"))  # dropped
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_r")

    def test_media_key_captured_when_allowed(self):
        # Experimental path (#258 part 2): media keys captured like any key.
        c = ChordCapture(allow_media=True)
        c.press(_Key("media_play_pause"))
        self.assertEqual(c.release(_Key("media_play_pause")), "media_play_pause")

    def test_lone_media_key_dropped_yields_nothing(self):
        c = ChordCapture()  # media not allowed
        c.press(_Key("media_play_pause"))
        # Nothing bindable was ever held → release emits nothing, not done.
        self.assertIsNone(c.release(_Key("media_play_pause")))
        self.assertFalse(c.done)


class ResolvedRoundTripTests(unittest.TestCase):
    """The captured chord names must be resolvable back by the PTT backends, so
    a binding produced by capture is one the listener can actually arm. This
    guards the family→concrete-name mapping against drift."""

    def test_modifier_names_are_bare_modifier_bindings(self):
        from whisper_dictate.vp_keys_solo import is_bare_modifier_binding
        for fam in ("ctrl_r", "shift_r", "alt_r"):
            self.assertTrue(
                is_bare_modifier_binding([fam]),
                f"{fam} should be recognised as a bare-modifier binding")
        self.assertTrue(
            is_bare_modifier_binding(["ctrl_r", "shift_r"]),
            "a captured modifier chord stays a bare-modifier binding")

    def test_module_exports(self):
        # Smoke: the public surface the IO shell + CLI import is present.
        self.assertTrue(hasattr(vp_keys_capture, "ChordCapture"))
        self.assertTrue(hasattr(vp_keys_capture, "key_to_setting_name"))
        self.assertTrue(hasattr(vp_keys_capture, "canonical_chord"))


def _scripted(answers):
    """An input_fn returning scripted answers in order, then '' (ENTER)."""
    it = iter(answers)

    def _input(_prompt):
        try:
            return next(it)
        except StopIteration:
            return ""

    return _input


class CaptureHotkeyFlowTests(unittest.TestCase):
    """The press-to-capture CLI orchestration (vp_keys_capture_cli, #258 part 1).

    The chord assembly is tested above; here the prompt / confirmation / persist
    wiring is driven with an injected capture_fn + config_writer so no keyboard
    or real disk is touched."""

    def setUp(self):
        from whisper_dictate import vp_keys_capture_cli
        self.cli = vp_keys_capture_cli

    def test_confirm_saves_to_key_setting(self):
        lines = []
        written = {}

        def _writer(cfg):
            written.update(cfg)
            return "config.json"

        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: "shift_r+ctrl_r",
            input_fn=_scripted(["y"]),
            output_fn=lines.append,
            config_writer=_writer,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(written.get("key"), "shift_r+ctrl_r")
        joined = "\n".join(lines)
        self.assertIn("shift_r+ctrl_r", joined)
        self.assertIn("VOICEPI_KEY", joined)  # equivalent env line printed

    def test_decline_does_not_write(self):
        lines = []
        calls = {"n": 0}

        def _writer(cfg):
            calls["n"] += 1
            return "config.json"

        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: "ctrl_r",
            input_fn=_scripted(["n"]),
            output_fn=lines.append,
            config_writer=_writer,
        )
        self.assertEqual(rc, 1)
        self.assertEqual(calls["n"], 0, "declining must not write config")
        self.assertIn("Discarded", "\n".join(lines))

    def test_nothing_captured_returns_nonzero(self):
        lines = []
        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: None,  # timeout / nothing held
            input_fn=_scripted([]),
            output_fn=lines.append,
            config_writer=lambda cfg: "x",
        )
        self.assertEqual(rc, 1)
        self.assertIn("No hotkey captured", "\n".join(lines))

    def test_allow_media_and_timeout_forwarded_to_capture(self):
        seen = {}

        def _capture(**kw):
            seen.update(kw)
            return "media_play_pause"

        self.cli.run_capture_hotkey(
            capture_fn=_capture,
            input_fn=_scripted(["y"]),
            output_fn=lambda _t="": None,
            config_writer=lambda cfg: "x",
            allow_media=True,
        )
        self.assertTrue(seen.get("allow_media"), "allow_media must reach capture_fn")
        self.assertIn("timeout", seen, "timeout must reach capture_fn")

    def test_persist_hotkey_preserves_other_settings(self):
        from unittest.mock import patch
        written = {}

        def _writer(cfg):
            written.update(cfg)
            return "x"

        with patch("whisper_dictate.vp_config.load_config",
                   return_value={"model": "small", "key": "ctrl_r"}):
            self.cli.persist_hotkey("alt_r", config_writer=_writer)
        self.assertEqual(written.get("model"), "small", "other keys kept")
        self.assertEqual(written.get("key"), "alt_r", "key overwritten")


class CaptureHotkeyCliDispatchTests(unittest.TestCase):
    def test_parser_accepts_capture_flags(self):
        from whisper_dictate import vp_cli
        ns = vp_cli.build_arg_parser().parse_args(
            ["--capture-hotkey", "--capture-hotkey-allow-media"])
        self.assertTrue(ns.capture_hotkey)
        self.assertTrue(ns.capture_hotkey_allow_media)

    def test_dispatch_capture_hotkey_calls_run_and_exits(self):
        from whisper_dictate import runtime
        from unittest.mock import patch
        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--capture-hotkey"])
        with patch("whisper_dictate.vp_keys_capture_cli.run_capture_hotkey",
                   return_value=0) as m:
            with self.assertRaises(SystemExit) as ctx:
                runtime._run_utility_subcommands(a, ap)
        self.assertEqual(ctx.exception.code, 0)
        m.assert_called_once_with(allow_media=False)

    def test_dispatch_forwards_allow_media(self):
        from whisper_dictate import runtime
        from unittest.mock import patch
        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--capture-hotkey", "--capture-hotkey-allow-media"])
        with patch("whisper_dictate.vp_keys_capture_cli.run_capture_hotkey",
                   return_value=0) as m:
            with self.assertRaises(SystemExit):
                runtime._run_utility_subcommands(a, ap)
        m.assert_called_once_with(allow_media=True)

    def test_allow_media_without_capture_hotkey_errors(self):
        from whisper_dictate import runtime
        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--capture-hotkey-allow-media"])
        with self.assertRaises(SystemExit) as ctx:
            runtime._run_utility_subcommands(a, ap)
        self.assertNotEqual(ctx.exception.code, 0)

    def test_capture_cli_import_pulls_no_ml_deps(self):
        # --capture-hotkey runs before ML modules load; importing its CLI module
        # must not pull torch/whisper/numpy/pynput (the listener imports lazily).
        import subprocess
        import sys as _sys
        from pathlib import Path as _Path
        src = str(_Path(__file__).resolve().parents[1])
        code = (
            "import sys; import whisper_dictate.vp_keys_capture_cli; "
            "bad=[m for m in ('torch','faster_whisper','ctranslate2','numpy',"
            "'sounddevice','pynput') if m in sys.modules]; print(','.join(bad))"
        )
        import os as _os
        env = dict(_os.environ, PYTHONPATH=src)
        out = subprocess.run([_sys.executable, "-c", code], capture_output=True,
                             text=True, env=env)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "", f"deps leaked: {out.stdout!r}")


if __name__ == "__main__":
    unittest.main()
