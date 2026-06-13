"""Tests for the headless config UX: --setup wizard + --export-config.

These exercise vp_setup directly (pure formatters + the stream-injectable wizard
engine) plus the CLI flag parsing/dispatch — never loading an STT model.
"""
from __future__ import annotations

import contextlib
import io

from helpers import json, os, sys, tempfile, unittest, Path

from whisper_dictate import vp_setup


@contextlib.contextmanager
def _clean_voicepi_env(**overrides):
    """Clear every VOICEPI_* / *_API_KEY env var so effective_config() is
    deterministic regardless of the developer's shell, then apply overrides."""
    saved = {
        k: os.environ.pop(k)
        for k in list(os.environ)
        if k.startswith("VOICEPI_") or k.endswith("_API_KEY")
    }
    for k, v in overrides.items():
        if v is not None:
            os.environ[k] = v
    try:
        yield
    finally:
        for k in [k for k in os.environ if k.startswith("VOICEPI_") or k.endswith("_API_KEY")]:
            os.environ.pop(k, None)
        os.environ.update(saved)


def _scripted(answers):
    """An input_fn that returns scripted answers in order, then '' (ENTER)."""
    it = iter(answers)

    def _input(_prompt):
        try:
            return next(it)
        except StopIteration:
            return ""

    return _input


def _collector():
    """An output_fn that appends to a list; returns (fn, lines)."""
    lines: list[str] = []
    return (lambda text="": lines.append(text)), lines


class PureFormatterTests(unittest.TestCase):
    def test_config_json_is_schema_ordered_and_parseable(self):
        cfg = {"lang": "en", "key": "f9", "model": "small"}
        text = vp_setup.format_config_json(cfg)
        parsed = json.loads(text)
        self.assertEqual(parsed, {"key": "f9", "model": "small", "lang": "en"})
        # schema order: key before model before lang
        self.assertLess(text.index("key"), text.index("model"))
        self.assertLess(text.index("model"), text.index("lang"))

    def test_powershell_lines(self):
        out = vp_setup.format_powershell_lines({"key": "f9", "model": "small"})
        self.assertIn("$env:VOICEPI_KEY = 'f9'", out)
        self.assertIn("$env:VOICEPI_MODEL = 'small'", out)

    def test_bash_lines_quote_special_values(self):
        out = vp_setup.format_bash_lines(
            {"key": "f9", "temperature": "0.0,0.2"}
        )
        self.assertIn("export VOICEPI_KEY=f9", out)  # safe chars: bare
        self.assertIn("export VOICEPI_TEMPERATURE='0.0,0.2'", out)  # comma quoted

    def test_bash_lines_escape_single_quote(self):
        out = vp_setup.format_bash_lines({"initial_prompt": "it's a test"})
        self.assertIn("export VOICEPI_INITIAL_PROMPT='it'\\''s a test'", out)

    def test_format_export_redacts_secrets_by_default(self):
        out = vp_setup.format_export(
            {"stt_backend": "openai"},
            {"VOICEPI_STT_API_KEY": "sk-real-secret"},
            include_secrets=False,
        )
        self.assertNotIn("sk-real-secret", out)
        self.assertIn("***", out)
        self.assertIn("secrets redacted", out)
        # all three sections present
        self.assertIn("# === config.json ===", out)
        self.assertIn("# === PowerShell ===", out)
        self.assertIn("# === bash ===", out)

    def test_format_export_includes_secrets_when_asked(self):
        out = vp_setup.format_export(
            {"stt_backend": "openai"},
            {"VOICEPI_STT_API_KEY": "sk-real-secret"},
            include_secrets=True,
        )
        self.assertIn("sk-real-secret", out)
        self.assertNotIn("secrets redacted", out)


class WizardBasicTrackTests(unittest.TestCase):
    def setUp(self):
        # Isolate the config path so effective_config() sees a clean baseline.
        self._cfg = os.path.join(tempfile.gettempdir(), "wd-wizard-test.json")
        try:
            os.remove(self._cfg)
        except OSError:
            pass

    def _run(self, answers, existing=None):
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(self._cfg)

        # No getpass calls expected unless backend=openai/post=cloud.
        rc = vp_setup.run_setup(
            input_fn=_scripted(answers),
            output_fn=out_fn,
            getpass_fn=lambda _p: "",
            config_writer=_writer,
        )
        return rc, captured.get("config", {}), "\n".join(lines)

    def test_all_enter_keeps_defaults_empty_config(self):
        # Basic track = all ENTER, then 'n' to advanced => no non-default keys.
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_KEY=None, VOICEPI_MODEL=None,
                  VOICEPI_STT_BACKEND=None, VOICEPI_DEVICE=None, VOICEPI_LANG=None,
                  VOICEPI_INJECT_MODE=None, VOICEPI_AUDIO_DEVICE=None,
                  VOICEPI_COMPUTE_TYPE=None):
            rc, config, _out = self._run([""] * 20 + ["n"])
        self.assertEqual(rc, 0)
        # minimal mode: only keys differing from default persist => none here
        self.assertEqual(config, {})

    def test_choosing_a_value_persists_it(self):
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_KEY=None, VOICEPI_MODEL=None,
                  VOICEPI_STT_BACKEND=None, VOICEPI_DEVICE=None, VOICEPI_LANG=None,
                  VOICEPI_INJECT_MODE=None, VOICEPI_AUDIO_DEVICE=None,
                  VOICEPI_COMPUTE_TYPE=None):
            # First basic prompt is `key`; set it to f9, ENTER the rest, no advanced
            rc, config, out = self._run(["f9"] + [""] * 19 + ["n"])
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("key"), "f9")
        self.assertIn("$env:VOICEPI_KEY = 'f9'", out)
        self.assertIn("export VOICEPI_KEY=f9", out)
        self.assertIn("Wrote config to:", out)

    def test_invalid_enum_reprompts(self):
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_STT_BACKEND=None):
            # key=ENTER, model=ENTER, stt_backend: 'banana' (invalid) then 'parakeet'
            rc, config, out = self._run(
                ["", "", "banana", "parakeet"] + [""] * 16 + ["n"]
            )
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("stt_backend"), "parakeet")
        self.assertIn("not one of", out)

    def test_cloud_backend_prompts_provider_and_key(self):
        captured_secret = {}

        def _getpass(_prompt):
            captured_secret["called"] = True
            return "sk-cloud-key"

        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(self._cfg)

        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_STT_BACKEND=None,
                  VOICEPI_STT_BASE_URL=None):
            # key=ENTER, model=ENTER, stt_backend=openai, rest of basic ENTER,
            # then cloud provider=groq, advanced=n
            answers = ["", "", "openai"] + [""] * 16 + ["groq", "n"]
            rc = vp_setup.run_setup(
                input_fn=_scripted(answers),
                output_fn=out_fn,
                getpass_fn=_getpass,
                config_writer=_writer,
            )
        out = "\n".join(lines)
        self.assertEqual(rc, 0)
        cfg = captured["config"]
        self.assertEqual(cfg.get("stt_backend"), "openai")
        # groq base url persisted (differs from openai default)
        self.assertEqual(cfg.get("stt_base_url"), "https://api.groq.com/openai/v1")
        self.assertTrue(captured_secret.get("called"))
        # secret never written to config, redacted in the printed env-lines
        self.assertNotIn("sk-cloud-key", out)
        self.assertIn("$env:VOICEPI_STT_API_KEY = '***'", out)


class WizardAdvancedTrackTests(unittest.TestCase):
    def setUp(self):
        self._cfg = os.path.join(tempfile.gettempdir(), "wd-wizard-adv.json")

    def _run(self, answers):
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(self._cfg)

        rc = vp_setup.run_setup(
            input_fn=_scripted(answers),
            output_fn=out_fn,
            getpass_fn=lambda _p: "",
            config_writer=_writer,
        )
        return rc, captured.get("config", {}), "\n".join(lines)

    def test_advanced_gated_off_by_default(self):
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg):
            # All basic ENTER, advanced => 'n'. beam_size (advanced) must stay absent.
            rc, config, _out = self._run([""] * 20 + ["n"])
        self.assertEqual(rc, 0)
        self.assertNotIn("beam_size", config)

    def test_advanced_yes_walks_advanced_track(self):
        # All basic ENTER, advanced => 'y'. The first advanced setting is
        # parakeet_model (free text); set it and confirm the advanced track ran
        # and the value persisted (basic-only would never reach it).
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_PARAKEET_MODEL=None):
            answers = [""] * 8 + ["y"] + ["nvidia/parakeet-tdt-0.6b-v3"] + [""] * 60
            rc, config, out = self._run(answers)
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("parakeet_model"), "nvidia/parakeet-tdt-0.6b-v3")
        self.assertIn("Local speech-to-text", out)  # advanced category header shown

    def test_numeric_out_of_bounds_reprompts(self):
        # beam_size has min=1, max=10. Drive a single-setting validation via the
        # wizard's validator to keep this independent of advanced ordering.
        from whisper_dictate import vp_setup as vs
        out_lines = []
        wiz = vs._Wizard(lambda _p: "", out_lines.append, existing={})
        row = next(r for r in vs._schema_rows() if r["key"] == "beam_size")
        choices = vs.ENUM_CHOICES.get("beam_size")
        self.assertIsNone(wiz._validate_answer("99", row, choices))   # > max
        self.assertIsNone(wiz._validate_answer("0", row, choices))    # < min
        self.assertEqual(wiz._validate_answer("4", row, choices), "4")  # valid
        self.assertTrue(any("invalid number" in m for m in out_lines))


class TtySafetyTests(unittest.TestCase):
    def test_non_tty_stdin_drives_from_scripted_lines(self):
        cfg = os.path.join(tempfile.gettempdir(), "wd-tty.json")
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(cfg)

        # A StringIO stdin is NOT a tty -> wizard reads readlines() instead of
        # blocking on input(). Feed: key=f9, everything else ENTER, advanced n.
        scripted = "f9\n" + "\n" * 19 + "n\n"
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg, VOICEPI_KEY=None):
            rc = vp_setup.run_setup(
                stdin=io.StringIO(scripted),
                output_fn=out_fn,
                getpass_fn=lambda _p: "",
                config_writer=_writer,
            )
        self.assertEqual(rc, 0)
        self.assertEqual(captured["config"].get("key"), "f9")

    def test_non_tty_secret_read_from_scripted_stdin(self):
        # Regression: getpass.getpass reads /dev/tty (not the pipe) and would
        # hang/fail on a non-TTY. The secret line must come from scripted stdin.
        cfg = os.path.join(tempfile.gettempdir(), "wd-tty-secret.json")
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(cfg)

        # key=ENTER, model=ENTER, stt_backend=openai, 5 more basic ENTER,
        # cloud provider=groq, advanced=n, then the secret line.
        scripted = "\n\nopenai\n\n\n\n\n\ngroq\nn\nsk-piped-key\n"
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg, VOICEPI_STT_BACKEND=None,
                                VOICEPI_STT_BASE_URL=None):
            rc = vp_setup.run_setup(
                stdin=io.StringIO(scripted),
                output_fn=out_fn,
                config_writer=_writer,
            )
        out = "\n".join(lines)
        self.assertEqual(rc, 0)
        # secret consumed from stdin, redacted in output, never in config.json
        self.assertNotIn("sk-piped-key", out)
        self.assertIn("$env:VOICEPI_STT_API_KEY = '***'", out)
        self.assertNotIn("stt_api_key", captured["config"])


class ExportTests(unittest.TestCase):
    def test_export_merges_config_and_env(self):
        cfg = os.path.join(tempfile.gettempdir(), "wd-export.json")
        Path(cfg).write_text(json.dumps({"model": "small"}), encoding="utf-8")
        out_fn, lines = _collector()
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg, VOICEPI_LANG="de"):
            rc = vp_setup.run_export(output_fn=out_fn, env={})
        out = "\n".join(lines)
        self.assertEqual(rc, 0)
        # config.json value
        self.assertIn('"model": "small"', out)
        # env override surfaced as effective config
        self.assertIn('"lang": "de"', out)
        self.assertIn("export VOICEPI_LANG=de", out)

    def test_export_redacts_secret_env_by_default(self):
        cfg = os.path.join(tempfile.gettempdir(), "wd-export2.json")
        Path(cfg).write_text("{}", encoding="utf-8")
        out_fn, lines = _collector()
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg):
            rc = vp_setup.run_export(
                output_fn=out_fn,
                env={"VOICEPI_STT_API_KEY": "sk-xyz"},
            )
        out = "\n".join(lines)
        self.assertEqual(rc, 0)
        self.assertNotIn("sk-xyz", out)
        self.assertIn("***", out)

    def test_export_include_secrets_emits_full_key(self):
        cfg = os.path.join(tempfile.gettempdir(), "wd-export3.json")
        Path(cfg).write_text("{}", encoding="utf-8")
        out_fn, lines = _collector()
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg):
            rc = vp_setup.run_export(
                output_fn=out_fn,
                include_secrets=True,
                env={"VOICEPI_POST_API_KEY": "post-secret"},
            )
        out = "\n".join(lines)
        self.assertEqual(rc, 0)
        self.assertIn("post-secret", out)

    def test_resolve_secret_envs_only_keeps_set_keys(self):
        got = vp_setup.resolve_secret_envs(
            {"VOICEPI_STT_API_KEY": "k", "GROQ_API_KEY": "", "UNRELATED": "x"}
        )
        self.assertEqual(got, {"VOICEPI_STT_API_KEY": "k"})


class CliDispatchTests(unittest.TestCase):
    """--setup / --export-config / --include-secrets parse and dispatch without
    loading a model."""

    def test_parser_accepts_new_flags(self):
        from whisper_dictate import vp_cli
        ns = vp_cli.build_arg_parser().parse_args(
            ["--export-config", "--include-secrets"]
        )
        self.assertTrue(ns.export_config)
        self.assertTrue(ns.include_secrets)
        ns2 = vp_cli.build_arg_parser().parse_args(["--setup"])
        self.assertTrue(ns2.setup)

    def test_dispatch_setup_calls_run_setup_and_exits(self):
        from whisper_dictate import runtime
        from unittest.mock import patch as _patch

        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--setup"])
        with _patch("whisper_dictate.vp_setup.run_setup", return_value=0) as m:
            with self.assertRaises(SystemExit) as ctx:
                runtime._run_utility_subcommands(a, ap)
        self.assertEqual(ctx.exception.code, 0)
        m.assert_called_once()

    def test_dispatch_export_passes_include_secrets(self):
        from whisper_dictate import runtime
        from unittest.mock import patch as _patch

        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--export-config", "--include-secrets"])
        with _patch("whisper_dictate.vp_setup.run_export", return_value=0) as m:
            with self.assertRaises(SystemExit):
                runtime._run_utility_subcommands(a, ap)
        m.assert_called_once_with(include_secrets=True)

    def test_vp_setup_import_pulls_no_ml_deps(self):
        # The config-UX path must NOT import torch/faster-whisper/numpy — it runs
        # before _load_runtime_modules. Import in a fresh subprocess and assert.
        import subprocess as _sp
        src = str(Path(__file__).resolve().parents[1])
        code = (
            "import sys; import whisper_dictate.vp_setup; "
            "bad=[m for m in ('torch','faster_whisper','ctranslate2','numpy',"
            "'sounddevice','pynput') if m in sys.modules]; "
            "print(','.join(bad))"
        )
        env = dict(os.environ, PYTHONPATH=src)
        out = _sp.run([sys.executable, "-c", code], capture_output=True,
                      text=True, env=env)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "", f"ML deps leaked: {out.stdout!r}")


if __name__ == "__main__":
    unittest.main()
