"""Tests for the library-first push-to-talk simulator.

Covers three layers, from smallest to widest:

1. The :func:`simulate_ptt` library function against a stubbed model and the
   real dictionary/postprocess/format stages — asserts the returned struct
   carries the transcript through to the dry-run output.
2. The argparse surface on :mod:`whisper_dictate.vp_cli` — the
   ``--simulate-ptt`` flag and its friends parse into the expected
   ``args`` namespace so no regression sneaks in.
3. End-to-end CLI dispatch through :mod:`whisper_dictate.runtime` with the
   model + heavy dependencies stubbed — proves the ``--simulate-ptt`` code
   path is actually reachable from ``python -m whisper_dictate.runtime``.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from helpers import (
    _capture_stdout,
    load_voice_pi_realnp,
    real_numpy,
)


FIXTURE = Path(__file__).resolve().parent / "fixtures" / "hello.wav"


def _fake_transcribe_result(text: str, *, raw_text: str | None = None):
    """Build a TranscribeResult-like namespace with the given transcript."""
    return types.SimpleNamespace(
        text=text,
        raw_text=raw_text if raw_text is not None else text,
        duration_s=0.5,
        post_boost_dbfs=-20.0,
        raw_dbfs=-30.0,
        peak=0.5,
        gain=1.0,
        noise_dbfs=-70.0,
        snr_db=40.0,
        input_status="good",
        compute_s=0.05,
        real_time_factor=0.1,
        language="en",
        language_probability=0.99,
        gate="raw=-30dBFS noise=-70dBFS snr=40dB input=good",
        segments=[],
        dictionary_terms=[],
        dictionary_replacements=[],
    )


def _passthrough_postprocess(text, _settings=None):
    return types.SimpleNamespace(
        text=text,
        raw_text=text,
        provider="none",
        mode="raw",
        model="",
        latency_ms=0,
        changed=False,
        fallback=False,
        error=None,
        redacted=False,
        redactions=[],
    )


class SimulatePttLibraryTests(unittest.TestCase):
    """Exercise ``simulate_ptt`` directly with a stub model + real fixture."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib

        cls.mod = importlib.import_module("whisper_dictate.vp_simulate_ptt")
        cls.vp_transcribe = importlib.import_module(
            "whisper_dictate.vp_transcribe")

    def test_fixture_exists(self):
        """Guard: the committed fixture WAV is present + non-empty."""
        self.assertTrue(FIXTURE.exists(),
                        f"fixture missing: {FIXTURE}")
        self.assertGreater(FIXTURE.stat().st_size, 1000)

    def test_dry_run_prints_would_inject_and_returns_transcript(self):
        """Full pipeline: stubbed model → dry-run output includes transcript."""
        text = "hello world from simulate ptt"

        def fake_transcribe(_model, _pcm, _lang):
            return _fake_transcribe_result(text)

        buf = io.StringIO()
        with patch.object(self.vp_transcribe, "_transcribe_detail",
                          fake_transcribe), \
                patch.object(self.mod, "postprocess_text",
                             _passthrough_postprocess, create=True), \
                contextlib.redirect_stdout(buf):
            result = self.mod.simulate_ptt(object(), str(FIXTURE), lang="en")

        self.assertEqual(result.text, text)
        self.assertFalse(result.injected)
        self.assertIsNone(result.inject_strategy)
        self.assertEqual(result.inject_mode, "dry-run")
        self.assertEqual(result.language, "en")
        self.assertEqual(result.wav_path, str(FIXTURE))
        output = buf.getvalue()
        self.assertIn("would inject", output)
        self.assertIn(text, output)

    def test_empty_transcription_prints_no_text_and_does_not_inject(self):
        """An empty transcript skips injection AND the dry-run print of it."""
        def fake_transcribe(_model, _pcm, _lang):
            return _fake_transcribe_result("")

        buf = io.StringIO()
        with patch.object(self.vp_transcribe, "_transcribe_detail",
                          fake_transcribe), \
                patch.object(self.mod, "postprocess_text",
                             _passthrough_postprocess, create=True), \
                contextlib.redirect_stdout(buf):
            result = self.mod.simulate_ptt(object(), str(FIXTURE))

        self.assertEqual(result.text, "")
        self.assertFalse(result.injected)
        output = buf.getvalue()
        self.assertIn("no text produced", output)
        self.assertNotIn("would inject", output)

    def test_inject_true_calls_pynput_controller(self):
        """--inject actually calls pynput's Controller().type(text)."""
        text = "typed via pynput"

        def fake_transcribe(_model, _pcm, _lang):
            return _fake_transcribe_result(text)

        typed = []

        class FakeController:
            def type(self, s):  # noqa: A003 — match pynput API
                typed.append(s)

        fake_keyboard = types.SimpleNamespace(Controller=FakeController)
        fake_pynput = types.ModuleType("pynput")
        fake_pynput.keyboard = fake_keyboard
        sys.modules["pynput"] = fake_pynput
        sys.modules["pynput.keyboard"] = fake_keyboard

        buf = io.StringIO()
        with patch.object(self.vp_transcribe, "_transcribe_detail",
                          fake_transcribe), \
                patch.object(self.mod, "postprocess_text",
                             _passthrough_postprocess, create=True), \
                contextlib.redirect_stdout(buf):
            result = self.mod.simulate_ptt(
                object(), str(FIXTURE), inject=True)

        self.assertEqual(typed, [text])
        self.assertTrue(result.injected)
        # POC-scope guard (Claude review finding on PR #491): the reported
        # strategy must exactly reflect what actually ran. Only the direct-
        # typing pynput primitive is wired here; the label + inject_mode
        # follow.
        self.assertEqual(result.inject_strategy, "pynput-type")
        self.assertEqual(result.inject_mode, "type")
        self.assertIn("injected", buf.getvalue())

    def test_to_event_serialises_all_fields(self):
        """The JSON payload the CLI emits includes every result field."""
        result = self.mod.SimulatePttResult(
            text="hi",
            raw_text="hi",
            dictionary_text="hi",
            post_text="hi",
            injected=False,
            inject_strategy=None,
            inject_mode="dry-run",
            language="en",
            audio_duration_s=0.5,
            compute_s=0.05,
            wav_path="hello.wav",
        )
        event = result.to_event()
        self.assertEqual(event["event"], "simulate_ptt")
        for field in ("text", "raw_text", "dictionary_text", "post_text",
                      "injected", "inject_strategy", "inject_mode", "language",
                      "audio_duration_s", "compute_s", "wav_path",
                      "dictionary_replacements", "dictionary_terms"):
            self.assertIn(field, event)


class SimulatePttArgparseTests(unittest.TestCase):
    """The vp_cli argparse surface accepts the new flags without regressing."""

    def setUp(self):
        sys.modules.pop("vp_cli", None)
        sys.modules.pop("whisper_dictate.vp_cli", None)

    def test_parser_accepts_simulate_ptt_flags(self):
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--simulate-ptt", "--wav", "hello.wav",
        ])
        self.assertTrue(args.simulate_ptt)
        self.assertEqual(args.wav, "hello.wav")
        self.assertFalse(args.inject)

    def test_parser_accepts_inject_flag(self):
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--simulate-ptt", "--wav", "hi.wav", "--inject",
        ])
        self.assertTrue(args.simulate_ptt)
        self.assertTrue(args.inject)

    def test_parser_has_no_inject_mode_selector(self):
        """POC-scope guard (Claude review finding on PR #491): the CLI must
        not expose an `--inject-mode` selector until paste is implemented —
        otherwise `--inject-mode paste` silently runs the type strategy."""
        from whisper_dictate import vp_cli

        with self.assertRaises(SystemExit):
            vp_cli.build_arg_parser().parse_args([
                "--simulate-ptt", "--wav", "hi.wav",
                "--inject-mode", "paste",
            ])


class SimulatePttStandaloneParserTests(unittest.TestCase):
    """The standalone ``python -m whisper_dictate.vp_simulate_ptt`` parser."""

    @classmethod
    def setUpClass(cls):
        # Real numpy is enough — the standalone parser is a pure argparse.
        real_numpy()
        import importlib
        cls.mod = importlib.import_module("whisper_dictate.vp_simulate_ptt")

    def test_wav_is_required(self):
        with self.assertRaises(SystemExit):
            self.mod._build_parser().parse_args([])

    def test_dry_run_is_default(self):
        args = self.mod._build_parser().parse_args(["--wav", "hello.wav"])
        self.assertEqual(args.wav, "hello.wav")
        self.assertFalse(args.inject)

    def test_dry_run_and_inject_are_mutually_exclusive(self):
        # --dry-run and --inject may not be passed together (mutually exclusive
        # group). The default already IS dry-run, so users typically drop the
        # flag entirely; keeping the group prevents the surprising combination
        # `--dry-run --inject`.
        with self.assertRaises(SystemExit):
            self.mod._build_parser().parse_args([
                "--wav", "hello.wav", "--dry-run", "--inject",
            ])

    def test_standalone_parser_has_no_inject_mode_selector(self):
        """POC-scope guard (Claude review finding on PR #491): the standalone
        parser must not expose `--inject-mode` either — otherwise `--inject
        --inject-mode paste` would silently type."""
        with self.assertRaises(SystemExit):
            self.mod._build_parser().parse_args([
                "--wav", "hello.wav", "--inject-mode", "paste",
            ])

    def test_json_result_round_trips(self):
        result = self.mod.SimulatePttResult(
            text="hi", raw_text="hi", dictionary_text="hi", post_text="hi",
            injected=False, inject_strategy=None, inject_mode="dry-run",
            language="en", audio_duration_s=0.5, compute_s=0.05,
            wav_path="hello.wav",
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.mod._print_result(result, as_json=True)
        # One JSON line, parseable, with the event tag set.
        payload = json.loads(buf.getvalue())
        self.assertEqual(payload["event"], "simulate_ptt")
        self.assertEqual(payload["text"], "hi")


class SimulatePttStandaloneMainTests(unittest.TestCase):
    """Cover the ``python -m whisper_dictate.vp_simulate_ptt`` ``main`` entry.

    A second Claude review finding on PR #491 flagged ``main()`` +
    ``_load_model_for_cli`` as new production code with no coverage — a real
    gap because the standalone entry point is a separate surface from both
    the runtime-dispatched flow (covered by
    :class:`SimulatePttRuntimeDispatchTests`) and the argparse-in-isolation
    tests. Fix that here.
    """

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()

    def test_main_loads_model_calls_simulate_and_returns_zero(self):
        """``main`` resolves device+model, calls simulate_ptt, prints result."""
        from whisper_dictate import vp_simulate_ptt, vp_transcribe

        loaded_model = object()
        load_calls = []

        def fake_load(model_name, device, compute_type):
            load_calls.append((model_name, device, compute_type))
            return loaded_model

        seen = {}

        def fake_simulate(model, wav, *, lang, inject):
            seen["model"] = model
            seen["wav"] = wav
            seen["lang"] = lang
            seen["inject"] = inject
            return vp_simulate_ptt.SimulatePttResult(
                text="hello world",
                raw_text="hello world",
                dictionary_text="hello world",
                post_text="hello world",
                injected=False,
                inject_strategy=None,
                inject_mode="dry-run",
                language=lang or "auto",
                audio_duration_s=0.5,
                compute_s=0.05,
                wav_path=wav,
            )

        with patch.object(vp_transcribe, "load_stt_model", fake_load), \
                patch.object(vp_simulate_ptt, "simulate_ptt", fake_simulate), \
                _capture_stdout() as buf:
            rc = vp_simulate_ptt.main([
                "--wav", str(FIXTURE),
                "--lang", "en",
                "--model", "tiny.en",
                "--device", "cpu",
            ])

        self.assertEqual(rc, 0)
        self.assertEqual(seen["wav"], str(FIXTURE))
        self.assertEqual(seen["lang"], "en")
        self.assertFalse(seen["inject"])
        self.assertIs(seen["model"], loaded_model)
        # load_stt_model was called exactly once with the resolved model + a
        # cpu device (--device cpu forces `("cpu", "int8")` via _resolve_device).
        self.assertEqual(len(load_calls), 1)
        self.assertEqual(load_calls[0][0], "tiny.en")
        self.assertEqual(load_calls[0][1], "cpu")
        # Default (non-JSON) print is the transcript text on its own line.
        self.assertIn("hello world", buf.getvalue())

    def test_main_json_flag_emits_parseable_json_event(self):
        """--json makes the standalone entry emit the SimulatePttResult event."""
        from whisper_dictate import vp_simulate_ptt, vp_transcribe

        def fake_simulate(model, wav, *, lang, inject):
            return vp_simulate_ptt.SimulatePttResult(
                text="hi", raw_text="hi", dictionary_text="hi", post_text="hi",
                injected=False, inject_strategy=None, inject_mode="dry-run",
                language="en", audio_duration_s=0.5, compute_s=0.05,
                wav_path=wav,
            )

        with patch.object(vp_transcribe, "load_stt_model",
                          lambda *a, **kw: object()), \
                patch.object(vp_simulate_ptt, "simulate_ptt", fake_simulate), \
                _capture_stdout() as buf:
            rc = vp_simulate_ptt.main([
                "--wav", str(FIXTURE), "--json",
                "--model", "tiny.en", "--device", "cpu",
            ])

        self.assertEqual(rc, 0)
        payload = json.loads(buf.getvalue())
        self.assertEqual(payload["event"], "simulate_ptt")
        self.assertEqual(payload["text"], "hi")


class SimulatePttRuntimeDispatchTests(unittest.TestCase):
    """End-to-end: `--simulate-ptt` on the runtime CLI reaches the simulator."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()

    def test_runtime_dispatches_simulate_ptt_and_exits_zero(self):
        """`main` sees --simulate-ptt, calls simulate_ptt, exits 0."""
        from whisper_dictate import vp_simulate_ptt, vp_transcribe

        argv = ["prog", "--simulate-ptt", "--wav", str(FIXTURE), "--lang", "en"]
        called = {}

        def fake_simulate_ptt(model, wav, *, lang, inject):
            called["model"] = model
            called["wav"] = wav
            called["lang"] = lang
            called["inject"] = inject
            return vp_simulate_ptt.SimulatePttResult(
                text="hello",
                raw_text="hello",
                dictionary_text="hello",
                post_text="hello",
                injected=False,
                inject_strategy=None,
                inject_mode="dry-run",
                language="en",
                audio_duration_s=0.5,
                compute_s=0.05,
                wav_path=wav,
            )

        # Stub the heavy piece the runtime touches on the model-load path so
        # this test stays hermetic — a real load_stt_model would try to fetch
        # a multi-GB Whisper model from HuggingFace on a cold CI runner.
        fake_load = lambda *_args, **_kwargs: object()  # noqa: E731

        with patch.object(vp_simulate_ptt, "simulate_ptt",
                          side_effect=fake_simulate_ptt), \
                patch.object(vp_transcribe, "load_stt_model", fake_load), \
                patch.object(sys, "argv", argv), \
                _capture_stdout() as out:
            with self.assertRaises(SystemExit) as ctx:
                self.runtime.main()

        self.assertEqual(ctx.exception.code, 0)
        self.assertEqual(called["wav"], str(FIXTURE))
        self.assertEqual(called["lang"], "en")
        self.assertFalse(called["inject"])
        self.assertIn("hello", out.getvalue())


if __name__ == "__main__":
    unittest.main()
