"""Unit tests for vp_format (extracted from runtime.py).

Voice format commands run through the Rust ``format-text`` helper. These pin the
command-set normalisation, the off-by-default short circuit, and the helper
shell-out result parsing / error handling (with the helper subprocess stubbed).
"""
import subprocess

from helpers import (
    _env,
    patch,
    unittest,
)

from whisper_dictate import vp_format


class NormalizeCommandSetTests(unittest.TestCase):
    def test_off_aliases(self):
        for raw in (None, "", "off", "0", "false", "no"):
            self.assertEqual(vp_format._normalize_format_command_set(raw), "off")

    def test_explicit_languages_pass_through(self):
        self.assertEqual(vp_format._normalize_format_command_set("en"), "en")
        self.assertEqual(vp_format._normalize_format_command_set("da"), "da")
        self.assertEqual(vp_format._normalize_format_command_set("both"), "both")

    def test_all_and_truthy_map_to_both(self):
        self.assertEqual(vp_format._normalize_format_command_set("all"), "both")
        self.assertEqual(vp_format._normalize_format_command_set("1"), "both")
        self.assertEqual(vp_format._normalize_format_command_set("yes"), "both")
        self.assertEqual(vp_format._normalize_format_command_set("on"), "both")


class ApplyFormatCommandsTests(unittest.TestCase):
    def test_off_short_circuits_without_helper(self):
        # Off by default (empty test config); no helper needed, text passes through.
        with _env(VOICEPI_FORMAT_COMMANDS=None, VOICEPI_RUST_INJECTOR=None):
            result = vp_format.apply_format_commands("write comma literally")
        self.assertFalse(result.enabled)
        self.assertEqual(result.command_set, "off")
        self.assertEqual(result.text, "write comma literally")

    def test_enabled_requires_helper(self):
        with _env(VOICEPI_RUST_INJECTOR=None), \
                self.assertRaisesRegex(RuntimeError, "Rust format-text helper"):
            vp_format.apply_format_commands("first comma", "en")

    def test_helper_failure_surfaces_stderr(self):
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 1, stdout="", stderr="boom")
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_format.subprocess, "run", lambda *a, **k: completed):
            with self.assertRaisesRegex(RuntimeError, "boom"):
                vp_format.apply_format_commands("first comma", "en")

    def test_invalid_json_raises(self):
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout="not json", stderr="")
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_format.subprocess, "run", lambda *a, **k: completed):
            with self.assertRaisesRegex(RuntimeError, "invalid JSON"):
                vp_format.apply_format_commands("x", "en")

    def test_successful_result_is_parsed(self):
        payload = (
            '{"text": "Hello, world", "enabled": true, "changed": true,'
            ' "command_set": "en",'
            ' "applied": [{"command": "comma", "replacement": ",", "count": 1}]}'
        )
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout=payload, stderr="")
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            return completed

        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_format.subprocess, "run", fake_run):
            result = vp_format.apply_format_commands("hello world comma", "en")

        self.assertTrue(result.enabled)
        self.assertTrue(result.changed)
        self.assertEqual(result.text, "Hello, world")
        self.assertEqual(result.command_set, "en")
        self.assertEqual(result.applied, [
            {"command": "comma", "replacement": ",", "count": "1"}])
        # The command-set is forwarded to the helper verbatim.
        self.assertIn("--command-set", captured["cmd"])
        self.assertIn("en", captured["cmd"])

    def test_helper_output_decoded_as_utf8_with_danish_text(self):
        # The Rust helper emits UTF-8 JSON; decoding via the Windows locale
        # (cp1252) would mangle Danish characters, so the call must pin utf-8.
        payload = (
            '{"text": "Goddag, æøå ÆØÅ", "enabled": true, "changed": true,'
            ' "command_set": "da", "applied": []}'
        )
        completed = subprocess.CompletedProcess(
            ["whisper-dictate"], 0, stdout=payload, stderr="")
        captured = {}

        def fake_run(cmd, **kwargs):
            captured.update(kwargs)
            return completed

        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_format.subprocess, "run", fake_run):
            result = vp_format.apply_format_commands("goddag", "da")

        self.assertEqual(captured.get("encoding"), "utf-8")
        self.assertEqual(result.text, "Goddag, æøå ÆØÅ")

    def test_explicit_command_set_overrides_config(self):
        # command_set="off" wins even if VOICEPI_FORMAT_COMMANDS is set.
        with _env(VOICEPI_FORMAT_COMMANDS="both"):
            result = vp_format.apply_format_commands("x", "off")
        self.assertFalse(result.enabled)
        self.assertEqual(result.command_set, "off")


if __name__ == "__main__":
    unittest.main()
