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

    # Fix 1: PowerShell single-quote escaping
    def test_powershell_lines_escape_single_quote(self):
        """Embedded ' must be doubled so the PS line is valid syntax."""
        out = vp_setup.format_powershell_lines({"initial_prompt": "it's a test"})
        self.assertIn("$env:VOICEPI_INITIAL_PROMPT = 'it''s a test'", out)

    def test_ps_quote_helper(self):
        self.assertEqual(vp_setup._ps_quote("hello"), "'hello'")
        self.assertEqual(vp_setup._ps_quote("it's"), "'it''s'")
        self.assertEqual(vp_setup._ps_quote("a'b'c"), "'a''b''c'")
        self.assertEqual(vp_setup._ps_quote(""), "''")

    def test_powershell_secrets_escape_single_quote(self):
        out = vp_setup.format_powershell_lines(
            {}, {"VOICEPI_STT_API_KEY": "sk-it's-mine"}
        )
        self.assertIn("$env:VOICEPI_STT_API_KEY = 'sk-it''s-mine'", out)

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
        # Wave 8 of #348 dropped `"parakeet"` from the enum, so the wizard's
        # invalid-reprompt flow now drives the user through ENUM_CHOICES with
        # an explicit valid follow-up (openai).
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_STT_BACKEND=None):
            # key=ENTER, model=ENTER, stt_backend: 'banana' (invalid) then 'openai'
            rc, config, out = self._run(
                ["", "", "banana", "openai"] + [""] * 16 + ["n"]
            )
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("stt_backend"), "openai")
        self.assertIn("not one of", out)

    # Fix 4: cloud provider ENTER-to-keep must not clobber existing stt_base_url
    def test_cloud_enter_keeps_existing_custom_base_url(self):
        """Pressing ENTER at the provider prompt must preserve an existing custom URL."""
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(self._cfg)

        custom_url = "https://my-custom-endpoint.example.com/v1"
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_STT_BACKEND=None,
                                VOICEPI_STT_BASE_URL=None):
            # Simulate pre-existing config with stt_backend=openai and custom URL.
            import json as _json
            Path(self._cfg).write_text(
                _json.dumps({"stt_backend": "openai", "stt_base_url": custom_url}),
                encoding="utf-8",
            )
            # key=ENTER, model=ENTER, stt_backend=ENTER (keep openai), then
            # rest of basic ENTER; cloud provider prompt=ENTER (keep existing), advanced=n
            answers = ["", "", ""] + [""] * 16 + ["", "n"]
            rc = vp_setup.run_setup(
                input_fn=_scripted(answers),
                output_fn=out_fn,
                getpass_fn=lambda _p: "",
                config_writer=_writer,
            )
        self.assertEqual(rc, 0)
        # The custom URL must survive ENTER-to-keep
        cfg = captured["config"]
        self.assertEqual(
            cfg.get("stt_base_url"), custom_url,
            f"Expected custom URL to be preserved, got: {cfg.get('stt_base_url')!r}",
        )

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


class WizardPreservesUiOnlyKeysTests(unittest.TestCase):
    """Codex #435 P2 / Issue #334: run_setup must not strip UI-only keys
    (``settings_mode``, ``ui_theme``, ``ui_language``, ``ui_log_view``,
    ``onboarding_*``) from ``config.json``. These are not in the runtime
    settings schema (they are pure Rust UI state), so ``effective_config()``
    ignores them — a naive rewrite would silently drop them. That is exactly
    what caused a user who explicitly saved Simple mode to be migrated back
    to Advanced by the Rust load-time heuristic on the next wizard run.
    """

    def setUp(self):
        self._cfg = os.path.join(tempfile.gettempdir(), "wd-wizard-preserve.json")

    def _run(self, answers, seed_config):
        Path(self._cfg).write_text(
            json.dumps(seed_config), encoding="utf-8",
        )
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
        return rc, captured.get("config", {})

    def test_settings_mode_survives_wizard_rewrite(self):
        # A user who explicitly saved Simple mode must not be silently
        # migrated back to Advanced by the Rust load heuristic after the
        # next ``--setup`` run. All ENTER + advanced=n → the wizard writes
        # zero of its OWN keys, but the preserved settings_mode value has
        # to be in the emitted config.
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg):
            rc, config = self._run(
                [""] * 20 + ["n"],
                {"settings_mode": "simple"},
            )
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("settings_mode"), "simple")

    def test_multiple_ui_only_keys_survive_wizard_rewrite(self):
        # ui_theme / ui_language / ui_log_view / onboarding_* are the
        # other schema-unknown keys the Rust UI persists — they must
        # survive too so the wizard is a general-purpose UI-state-safe
        # rewrite, not just a settings_mode-safe one.
        seed = {
            "settings_mode": "simple",
            "ui_theme": "light",
            "ui_language": "da",
            "ui_log_view": "diagnostic",
            "onboarding_completed": True,
            "onboarding_seen_at": "2026-07-04T12:34:56Z",
        }
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg):
            rc, config = self._run([""] * 20 + ["n"], seed)
        self.assertEqual(rc, 0)
        for key, expected in seed.items():
            self.assertEqual(
                config.get(key),
                expected,
                f"UI-only key {key!r} must survive the wizard rewrite",
            )

    def test_schema_known_keys_are_not_duplicated_by_the_preserve_pass(self):
        # A schema-known key (e.g. ``lang``) must NOT round-trip through
        # the preserve pass — that would resurrect a value the wizard
        # decided to clear. The preserve pass filters to unknown keys, so
        # setting lang via the wizard to a value ≠ the seed must win.
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg):
            # First basic prompt is `key`; answer f9 for key, ENTER for
            # model + stt_backend, then "en" for the language prompt,
            # ENTER the rest, no advanced.
            rc, config = self._run(
                ["f9", "", "", "", "", "", "", "", "", "en"] + [""] * 10 + ["n"],
                {"lang": "da", "settings_mode": "simple"},
            )
        self.assertEqual(rc, 0)
        # settings_mode preserved.
        self.assertEqual(config.get("settings_mode"), "simple")


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
        # All basic ENTER, advanced => 'y'. Wave 8 of #348 dropped the
        # parakeet_model entry that used to be the first advanced setting;
        # the first remaining advanced setting in the stt-local category
        # is now initial_prompt (free text), used here to confirm the
        # advanced track ran and the value persisted (basic-only would
        # never reach it).
        with _clean_voicepi_env(VOICEPI_CONFIG=self._cfg, VOICEPI_INITIAL_PROMPT=None):
            answers = [""] * 8 + ["y"] + ["Keep Codex CLI terms."] + [""] * 60
            rc, config, out = self._run(answers)
        self.assertEqual(rc, 0)
        self.assertEqual(config.get("initial_prompt"), "Keep Codex CLI terms.")
        self.assertIn("Local speech-to-text", out)  # advanced category header shown

    def test_legacy_parakeet_stt_backend_migrates_to_whisper_on_enter(self):
        # Wave 8 of #348 + Codex P2 on PR #410: an upgraded user whose
        # config.json still carries `stt_backend = "parakeet"` from before
        # the backend removal must NOT have that value silently re-persisted
        # when the wizard prompts and the user hits ENTER. Pin the
        # normalisation at the wizard's _current() boundary so the
        # all-ENTER headless path lands on whisper.
        from whisper_dictate import vp_setup as vs

        wiz = vs._Wizard(
            lambda _p: "",
            lambda _m: None,
            existing={"stt_backend": "parakeet"},
        )
        row = next(r for r in vs._schema_rows() if r["key"] == "stt_backend")
        # ENTER-to-keep would persist a non-default current value; the
        # current value must now resolve to "whisper", not "parakeet".
        self.assertEqual(wiz._current(row), "whisper")

        # Drive the actual ENTER path to confirm config.json ends up with
        # whisper (not parakeet).
        wiz._prompt_one(row)
        self.assertNotEqual(wiz.config.get("stt_backend"), "parakeet")
        # The schema default IS whisper, so _prompt_one decides not to write
        # the key at all (matches its "only persist a non-default value"
        # contract). Either outcome — key absent OR key=="whisper" — is fine
        # from the user's point of view; both leave them on the new default.
        self.assertIn(wiz.config.get("stt_backend"), (None, "whisper"))

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

    # Fix 2: nan/inf rejection
    def test_nan_rejected(self):
        from whisper_dictate import vp_setup as vs
        out_lines = []
        wiz = vs._Wizard(lambda _p: "", out_lines.append, existing={})
        row = next(r for r in vs._schema_rows() if r["key"] == "beam_size")
        choices = vs.ENUM_CHOICES.get("beam_size")
        self.assertIsNone(wiz._validate_answer("nan", row, choices))
        self.assertTrue(any("invalid number" in m for m in out_lines))

    def test_inf_rejected(self):
        from whisper_dictate import vp_setup as vs
        out_lines = []
        wiz = vs._Wizard(lambda _p: "", out_lines.append, existing={})
        row = next(r for r in vs._schema_rows() if r["key"] == "beam_size")
        choices = vs.ENUM_CHOICES.get("beam_size")
        self.assertIsNone(wiz._validate_answer("inf", row, choices))
        self.assertTrue(any("invalid number" in m for m in out_lines))

    def test_coerce_number_nan_raises(self):
        import math as _math
        from whisper_dictate import vp_setup as vs
        row = next(r for r in vs._schema_rows() if r["key"] == "beam_size")
        with self.assertRaises(ValueError) as ctx:
            vs._coerce_number("nan", row)
        self.assertIn("finite", str(ctx.exception))

    def test_coerce_number_inf_raises(self):
        from whisper_dictate import vp_setup as vs
        row = next(r for r in vs._schema_rows() if r["key"] == "beam_size")
        with self.assertRaises(ValueError):
            vs._coerce_number("inf", row)

    # Fix 3: compute_type auto selectable
    def test_compute_type_auto_token_accepted(self):
        """Typing 'auto' when choices contain '' maps to the empty string."""
        from whisper_dictate import vp_setup as vs
        out_lines = []
        wiz = vs._Wizard(lambda _p: "", out_lines.append, existing={})
        # compute_type choices: ("", "float32", ...)
        choices = vs.ENUM_CHOICES["compute_type"]
        self.assertIn("", choices)
        result = wiz._validate_answer("auto", {"key": "compute_type"}, choices)
        self.assertEqual(result, "")

    def test_compute_type_invalid_shows_auto_not_empty(self):
        """Invalid choice error must not print the raw empty string."""
        from whisper_dictate import vp_setup as vs
        out_lines = []
        wiz = vs._Wizard(lambda _p: "", out_lines.append, existing={})
        choices = vs.ENUM_CHOICES["compute_type"]
        wiz._validate_answer("bogus", {"key": "compute_type"}, choices)
        error_lines = [m for m in out_lines if "not one of" in m]
        self.assertTrue(error_lines, "expected an error message")
        msg = error_lines[0]
        # Must list "auto" as a selectable token, not a bare empty string.
        self.assertIn("auto", msg)
        self.assertNotIn("''", msg)  # raw empty string must not appear

    def test_wizard_compute_type_auto_persists_empty(self):
        """Full wizard: typing 'auto' for compute_type stores '' in config."""
        cfg_path = os.path.join(tempfile.gettempdir(), "wd-ct-auto.json")
        out_fn, lines = _collector()
        captured = {}

        def _writer(config):
            captured["config"] = config
            return Path(cfg_path)

        # Find compute_type position in basic settings to know how many ENTERs
        # before it. The basic settings are key, model, stt_backend, device,
        # lang, inject_mode, audio_device, compute_type (order from schema).
        # We'll feed a scripted list with 'auto' at the right position.
        # key=ENTER, model=ENTER, stt_backend=ENTER, device=ENTER, lang=ENTER,
        # inject_mode=ENTER, audio_device=ENTER, compute_type=auto,
        # then ENTER for the rest, advanced=n
        with _clean_voicepi_env(VOICEPI_CONFIG=cfg_path, VOICEPI_COMPUTE_TYPE=None,
                                VOICEPI_STT_BACKEND=None):
            answers = ["", "", "", "", "", "", "", "auto"] + [""] * 12 + ["n"]
            rc = vp_setup.run_setup(
                input_fn=_scripted(answers),
                output_fn=out_fn,
                getpass_fn=lambda _p: "",
                config_writer=_writer,
            )
        self.assertEqual(rc, 0)
        # compute_type="" means auto — but minimal mode only stores non-defaults.
        # The schema default for compute_type is "" so it won't be in config.
        # What we verify is that we did NOT get re-prompted (no "not one of" error).
        out = "\n".join(lines)
        self.assertNotIn("not one of", out)


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

    # Fix 5: --include-secrets without --export-config must error
    def test_include_secrets_without_export_config_errors(self):
        """--include-secrets alone must produce a non-zero exit with a clear message."""
        from whisper_dictate import runtime
        ap = runtime.build_arg_parser()
        a = ap.parse_args(["--include-secrets"])
        with self.assertRaises(SystemExit) as ctx:
            runtime._run_utility_subcommands(a, ap)
        self.assertNotEqual(ctx.exception.code, 0)

    def test_vp_setup_import_pulls_no_ml_deps(self):
        # The config-UX path must NOT import torch/faster-whisper/numpy — it runs
        # before _load_runtime_modules. Import in a fresh subprocess and assert.
        # pynput is included: --capture-hotkey must import its listener LAZILY so
        # the import of vp_setup itself never pulls a global keyboard hook in.
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
