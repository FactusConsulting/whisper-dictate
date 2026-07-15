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
    like the real enum — the distinction capture now records verbatim (the actual
    side), reversing the earlier side-insensitive collapse."""

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
    def test_modifier_records_actual_side(self):
        # Side-specific (reverses #258 part 1's collapse): the ACTUAL side pressed
        # is recorded verbatim, so left and right Ctrl bind to DISTINCT names.
        self.assertEqual(key_to_setting_name(_Key("ctrl_l")), "ctrl_l")
        self.assertEqual(key_to_setting_name(_Key("ctrl_r")), "ctrl_r")

    def test_generic_variant_falls_back_to_concrete_family_name(self):
        # Documented edge case: when only the SIDELESS generic variant is ever
        # seen (no side to record), fall back to a concrete family name that the
        # side-aware matcher then accepts for either side.
        self.assertEqual(key_to_setting_name(_Key("ctrl")), "ctrl_r")
        self.assertEqual(key_to_setting_name(_Key("shift")), "shift_r")
        self.assertEqual(key_to_setting_name(_Key("alt")), "alt_r")
        self.assertEqual(key_to_setting_name(_Key("cmd")), "cmd_r")

    def test_each_side_specific_modifier_records_its_side(self):
        self.assertEqual(key_to_setting_name(_Key("shift_l")), "shift_l")
        self.assertEqual(key_to_setting_name(_Key("shift_r")), "shift_r")
        self.assertEqual(key_to_setting_name(_Key("alt_l")), "alt_l")
        self.assertEqual(key_to_setting_name(_Key("alt_r")), "alt_r")
        self.assertEqual(key_to_setting_name(_Key("alt_gr")), "alt_gr")  # own name
        self.assertEqual(key_to_setting_name(_Key("cmd_l")), "cmd_l")
        self.assertEqual(key_to_setting_name(_Key("cmd_r")), "cmd_r")

    def test_named_special_key_passthrough(self):
        self.assertEqual(key_to_setting_name(_Key("f9")), "f9")
        self.assertEqual(key_to_setting_name(_Key("space")), "space")

    def test_letter_keycode_is_unbindable(self):
        # Single letters can't be bound (backends only resolve named keys), so
        # capture maps them to None and ignores them rather than writing an
        # unusable value that would crash at startup.
        self.assertIsNone(key_to_setting_name(_Char("a")))

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

    def test_letter_is_ignored_not_bound(self):
        # A letter is unbindable, so pressing/releasing only a letter captures
        # nothing — the user must hold a real modifier/special key.
        c = ChordCapture()
        c.press(_Char("a"))
        self.assertIsNone(c.release(_Char("a")))
        self.assertFalse(c.done)


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

    def test_release_captures_full_set_pre_release_snapshot(self):
        # Build a 3-key chord, then let go one at a time — the captured chord is
        # the full set held just before releases began, never eroded by partial
        # releases.
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("shift_r"))
        c.press(_Key("alt_r"))
        self.assertEqual(sorted(c.held_names()), ["alt_r", "ctrl_r", "shift_r"])
        self.assertIsNone(c.release(_Key("alt_r")))
        self.assertIsNone(c.release(_Key("ctrl_r")))
        self.assertEqual(c.release(_Key("shift_r")), "alt_r+ctrl_r+shift_r")

    def test_same_size_chord_change_uses_current_held_set(self):
        # Copilot finding 1: press Ctrl+Shift, release Ctrl, press Alt — the
        # held set is now Shift+Alt (same size as Ctrl+Shift, but different
        # keys). The snapshot must reflect the ACTUAL held set at first release,
        # not the stale Ctrl+Shift set. Expected chord: alt_r+shift_r.
        c = ChordCapture()
        c.press(_Key("ctrl_r"))
        c.press(_Key("shift_r"))
        # Now release Ctrl and press Alt: same size (2), but chord changed.
        self.assertIsNone(c.release(_Key("ctrl_r")))
        c.press(_Key("alt_r"))
        # held is now {shift_r, alt_r} — the snapshot must have been refreshed.
        self.assertIsNone(c.release(_Key("shift_r")))
        result = c.release(_Key("alt_r"))
        self.assertEqual(result, "alt_r+shift_r",
                         "snapshot must reflect Shift+Alt, not the stale Ctrl+Shift")

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

    def test_left_and_right_same_family_are_distinct_members(self):
        # Side-specific (reverses #254): holding BOTH left and right Ctrl now
        # records a TWO-member chord (each side verbatim), not a collapsed single
        # ctrl_r — the sides are distinct.
        c = ChordCapture()
        c.press(_Key("ctrl_l"))
        c.press(_Key("ctrl_r"))
        self.assertIsNone(c.release(_Key("ctrl_l")))
        self.assertEqual(c.release(_Key("ctrl_r")), "ctrl_l+ctrl_r")

    def test_generic_variant_dedupes_with_concrete_fallback(self):
        # A generic Key.ctrl falls back to "ctrl_r"; holding it together with a
        # real right Ctrl dedupes to a single ctrl_r member.
        c = ChordCapture()
        c.press(_Key("ctrl"))        # generic → "ctrl_r"
        c.press(_Key("ctrl_r"))      # also "ctrl_r"
        self.assertIsNone(c.release(_Key("ctrl")))
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

    def test_unbindable_snapshot_resets_then_accepts_next_chord(self):
        # Exercise the "chord resolved to nothing bindable: reset and keep going"
        # branch (vp_keys_capture.py release() lines 182-183). We reach it by
        # using a _Char whose .char is empty-string — key_to_setting_name returns
        # None for it, but press() also blocks it (returns early) so it never
        # enters _held. To force the snapshot to contain only unbindable tokens
        # we patch key_to_setting_name to allow the key into _held but return
        # None during the resolve step.
        from unittest.mock import patch
        # A sentinel token that passes the press() guard with a patched
        # key_to_setting_name, but is resolved as None for the final chord.
        sentinel = _Key("__unbindable__")
        original = vp_keys_capture.key_to_setting_name

        def _patched(k):
            # Allow the sentinel past the press() guard but return None at
            # resolution time so the snapshot resolves to an empty chord.
            if k is sentinel:
                return "__unbindable__"  # truthy → enters _held
            return original(k)

        def _resolved_none(k):
            # At chord-emit time _resolved calls key_to_setting_name; make the
            # sentinel resolve to None so the chord comes back empty.
            return None

        c = ChordCapture()
        with patch.object(vp_keys_capture, "key_to_setting_name", _patched):
            c.press(sentinel)
        # Now _held and _pre_release_snapshot contain sentinel.
        # Override key_to_setting_name for the release step so _resolved → {}.
        with patch.object(vp_keys_capture, "key_to_setting_name", _resolved_none):
            result = c.release(sentinel)
        # The "nothing bindable" branch reset the snapshot; the capture is not done.
        self.assertIsNone(result)
        self.assertFalse(c.done)
        # After reset, a fresh key press and release should still bind normally.
        c.press(_Key("f9"))
        self.assertEqual(c.release(_Key("f9")), "f9")


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


class ChordCaptureSideSpecificTests(unittest.TestCase):
    """Capture now records the ACTUAL side: a left chord stays left-specific."""

    def test_left_chord_captured_side_specific(self):
        c = ChordCapture()
        c.press(_Key("ctrl_l"))
        c.press(_Key("shift_l"))
        self.assertIsNone(c.release(_Key("ctrl_l")))
        self.assertEqual(c.release(_Key("shift_l")), "ctrl_l+shift_l")

    def test_mixed_side_chord_keeps_both_sides(self):
        c = ChordCapture()
        c.press(_Key("ctrl_l"))
        c.press(_Key("shift_r"))
        self.assertIsNone(c.release(_Key("ctrl_l")))
        self.assertEqual(c.release(_Key("shift_r")), "ctrl_l+shift_r")


class ResolvedRoundTripTests(unittest.TestCase):
    """The captured chord names must be resolvable back by the PTT backends, so
    a binding produced by capture is one the listener can actually arm. This
    guards the name mapping against drift."""

    def test_modifier_names_are_bare_modifier_bindings(self):
        from whisper_dictate.vp_keys_solo import is_bare_modifier_binding
        # Both sides of each family are recognised bare-modifier bindings, so a
        # side-specific captured binding arms the press-alone guard.
        for name in ("ctrl_l", "ctrl_r", "shift_l", "shift_r", "alt_l", "alt_r"):
            self.assertTrue(
                is_bare_modifier_binding([name]),
                f"{name} should be recognised as a bare-modifier binding")
        self.assertTrue(
            is_bare_modifier_binding(["ctrl_l", "shift_l"]),
            "a captured side-specific modifier chord stays a bare-modifier binding")

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

    def test_wayland_session_warns_about_pynput_listener(self):
        from unittest import mock
        lines = []
        with mock.patch.dict("os.environ", {"WAYLAND_DISPLAY": "wayland-0"}):
            self.cli.run_capture_hotkey(
                capture_fn=lambda **kw: "ctrl_r",
                input_fn=_scripted(["n"]),
                output_fn=lines.append,
                config_writer=lambda cfg: "x",
            )
        self.assertIn("Wayland", "\n".join(lines))

    def test_no_wayland_no_warning(self):
        from unittest import mock
        lines = []
        with mock.patch.dict("os.environ", {}, clear=True):
            self.cli.run_capture_hotkey(
                capture_fn=lambda **kw: "ctrl_r",
                input_fn=_scripted(["n"]),
                output_fn=lines.append,
                config_writer=lambda cfg: "x",
            )
        self.assertNotIn("Wayland", "\n".join(lines))

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

    def test_persist_hotkey_preserves_sibling_types(self):
        # Copilot finding 3: the old {k: str(v) for ...} call corrupted types.
        # A number, bool, and null sibling must survive persist_hotkey unchanged.
        from unittest.mock import patch
        written = {}

        def _writer(cfg):
            written.update(cfg)
            return "x"

        existing = {"model": "small", "count": 3, "flag": True, "empty": None}
        with patch("whisper_dictate.vp_config.load_config",
                   return_value=existing):
            self.cli.persist_hotkey("ctrl_r", config_writer=_writer)
        self.assertIsInstance(written.get("count"), int,
                              "integer sibling must stay int, not str")
        self.assertIs(written.get("flag"), True,
                      "bool sibling must stay bool, not str")
        self.assertIsNone(written.get("empty"),
                          "null sibling must stay None, not 'None'")
        self.assertEqual(written.get("key"), "ctrl_r", "hotkey written")

    def test_run_capture_hotkey_stdin_scripted_input(self):
        # Cover the _resolve_input_fn scripted-stdin path that is exercised when
        # neither input_fn nor a TTY is available: the function reads lines from
        # a provided stdin-like object.
        import io
        lines = []

        def _writer(cfg):
            return "x"

        stdin = io.StringIO("y\n")
        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: "ctrl_r",
            # No input_fn: let _resolve_input_fn pick up the scripted stdin.
            stdin=stdin,
            output_fn=lines.append,
            config_writer=_writer,
        )
        self.assertEqual(rc, 0, "scripted 'y' answer should accept and save")

    def test_run_capture_hotkey_stdin_exhausted_returns_empty(self):
        # Cover the StopIteration branch inside _scripted: when stdin has no
        # more lines, the scripted reader returns "" (treated as "no"), so the
        # capture is declined.
        import io
        stdin = io.StringIO("")   # no lines at all
        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: "ctrl_r",
            stdin=stdin,
            output_fn=lambda _t="": None,
            config_writer=lambda cfg: "x",
        )
        self.assertEqual(rc, 1, "empty stdin means declined (no 'y' was given)")

    def test_run_capture_hotkey_tty_stdin_uses_input_builtin(self):
        # Cover the TTY branch (line 64: return input) in _resolve_input_fn.
        # Fake a stdin that reports isatty()=True, then inject an input_fn so
        # the actual builtin input() is never called.
        import io
        from unittest.mock import patch

        class _FakeTTY(io.StringIO):
            def isatty(self):
                return True

        # The function returns the builtin `input` when isatty() is True.
        # We can't actually call builtin input() in tests, so patch it to
        # return "n" (decline).
        with patch("builtins.input", return_value="n"):
            rc = self.cli.run_capture_hotkey(
                capture_fn=lambda **kw: "ctrl_r",
                stdin=_FakeTTY(),
                # No input_fn: must pick up the TTY path.
                output_fn=lambda _t="": None,
                config_writer=lambda cfg: "x",
            )
        self.assertEqual(rc, 1, "TTY input returning 'n' should decline")

    def test_run_capture_hotkey_stdout_param(self):
        # Cover the `out = stdout or sys.stdout` path (line 98) and the _emit
        # inner function body: pass a stdout stream and no output_fn.
        import io
        from unittest.mock import patch
        buf = io.StringIO()

        def _writer(cfg):
            return "x"

        rc = self.cli.run_capture_hotkey(
            capture_fn=lambda **kw: "shift_r",
            input_fn=_scripted(["y"]),
            stdout=buf,
            config_writer=_writer,
        )
        self.assertEqual(rc, 0)
        self.assertIn("shift_r", buf.getvalue())


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
