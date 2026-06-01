from __future__ import annotations

import importlib
import io
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
import wave
from contextlib import redirect_stderr, contextmanager
from pathlib import Path
from unittest.mock import patch


_TEST_CONFIG = os.path.join(tempfile.gettempdir(), "whisper-dictate-test-config.json")
os.environ.setdefault("VOICEPI_CONFIG", _TEST_CONFIG)
try:
    os.remove(_TEST_CONFIG)
except OSError:
    pass


def load_voice_pi(cuda_devices: int = 0):
    for name in ("voice_pi", "vp_keymap", "vp_device", "vp_audio", "vp_inject",
                 "vp_cli", "vp_transcribe", "vp_dictionary", "vp_parakeet",
                 "vp_config", "vp_privacy", "vp_postprocess",
                 "ctranslate2", "faster_whisper", "numpy",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)

    ctranslate2 = types.ModuleType("ctranslate2")
    ctranslate2.get_cuda_device_count = lambda: cuda_devices
    sys.modules["ctranslate2"] = ctranslate2

    faster_whisper = types.ModuleType("faster_whisper")
    faster_whisper.WhisperModel = object
    sys.modules["faster_whisper"] = faster_whisper

    sys.modules["numpy"] = types.ModuleType("numpy")
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")

    pynput = types.ModuleType("pynput")
    keyboard = types.ModuleType("keyboard")
    keyboard.Controller = object
    keyboard.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(),
        shift_l=object(), shift_r=object(),
        alt_l=object(), alt_r=object(),
        esc=object(),
    )
    keyboard.Listener = object
    pynput.keyboard = keyboard
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = keyboard

    return importlib.import_module("voice_pi")


def load_voice_pi_realnp():
    """Import voice_pi with the REAL numpy (for audio-DSP tests) but the
    heavy/uninstalled deps stubbed. CI installs numpy (see tests workflow)."""
    for name in ("voice_pi", "vp_keymap", "vp_device", "vp_audio", "vp_inject",
                 "vp_cli", "vp_transcribe", "vp_dictionary", "vp_parakeet",
                 "vp_config", "vp_privacy", "vp_postprocess",
                 "ctranslate2", "faster_whisper",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)
    np_mod = sys.modules.get("numpy")
    if np_mod is not None and not hasattr(np_mod, "ndarray"):
        # a fake numpy left by another test — drop it so the real one loads
        for n in [m for m in list(sys.modules)
                  if m == "numpy" or m.startswith("numpy.")]:
            sys.modules.pop(n, None)
    import numpy  # noqa: F401 — real numpy must import (CI pip-installs it)

    ct = types.ModuleType("ctranslate2")
    ct.get_cuda_device_count = lambda: 0
    sys.modules["ctranslate2"] = ct
    fw = types.ModuleType("faster_whisper")
    fw.WhisperModel = object
    sys.modules["faster_whisper"] = fw
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")
    pynput = types.ModuleType("pynput")
    kb = types.ModuleType("keyboard")
    kb.Controller = object
    kb.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(), shift_l=object(),
        shift_r=object(), alt_l=object(), alt_r=object(), esc=object())
    kb.Listener = object
    pynput.keyboard = kb
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = kb
    return importlib.import_module("voice_pi")


@contextmanager
def _capture_stdout():
    import contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        yield buf


class AudioDspTests(unittest.TestCase):
    """Characterisation tests for the audio DSP with REAL numpy. These pin
    current behaviour so the upcoming vp_audio.py extraction is provably
    behaviour-preserving (same asserts, only the import path changes)."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.vp = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import numpy as np
        cls.np = np

    # --- _noise_snr ---
    def test_noise_snr_too_few_frames(self):
        a = self.np.zeros(1000, dtype=self.np.float32)
        self.assertEqual(self.vp._noise_snr(a), (-90.0, 0.0))

    def test_noise_snr_constant_signal(self):
        a = self.np.full(480 * 8, 0.5, dtype=self.np.float32)
        noise, snr = self.vp._noise_snr(a)
        self.assertAlmostEqual(noise, -6.0206, places=2)
        self.assertAlmostEqual(snr, 0.0, places=6)

    def test_noise_snr_contrast_has_high_snr(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 1.0 if i % 2 == 0 else 0.001, dtype=np.float32)
            for i in range(10)])
        noise, snr = self.vp._noise_snr(a)
        self.assertGreater(snr, 40.0)
        self.assertLess(noise, -40.0)

    # --- _boost_quiet ---
    def test_boost_quiet_normalises_toward_target(self):
        np = self.np
        a = np.full(1920, 0.01, dtype=np.float32)
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertEqual(out.dtype, np.float32)
        rms = float(np.sqrt(np.mean(out ** 2)))
        self.assertAlmostEqual(20 * np.log10(rms), self.vp.TARGET_DBFS,
                               places=1)

    def test_boost_quiet_never_clips(self):
        np = self.np
        a = np.zeros(1920, dtype=np.float32)
        a[:10] = 0.9
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertLessEqual(float(np.max(np.abs(out))), 0.99 + 1e-6)

    def test_cap_line_is_bold_on_interactive_terminal(self):
        import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR=None, VOICEPI_NO_COLOR=None):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "\033[1m[cap] raw=-20dBFS\033[0m",
                )

    def test_cap_line_stays_plain_for_piped_output(self):
        import vp_audio

        class Pipe:
            def isatty(self):
                return False

        with patch.object(vp_audio.sys, "stdout", Pipe()):
            self.assertEqual(
                vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                "[cap] raw=-20dBFS",
            )

    def test_cap_line_highlight_respects_no_color(self):
        import vp_audio

        class Tty:
            def isatty(self):
                return True

        with patch.object(vp_audio.sys, "stdout", Tty()):
            with _env(NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )
            with _env(VOICEPI_NO_COLOR="1"):
                self.assertEqual(
                    vp_audio._highlight_cap_line("[cap] raw=-20dBFS"),
                    "[cap] raw=-20dBFS",
                )

    # --- _looks_like_speech ---
    def test_looks_like_speech_rejects_too_quiet(self):
        a = self.np.full(1920, 1e-4, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("too quiet", msg)

    def test_looks_like_speech_rejects_flat_signal(self):
        a = self.np.full(1920, 0.1, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("no speech contrast", msg)

    def test_looks_like_speech_accepts_contrasted_speech(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(10)])
        ok, _ = self.vp._looks_like_speech(a)
        self.assertTrue(ok)


class WorkerEventTests(unittest.TestCase):
    def test_worker_events_disabled_by_default(self):
        import vp_worker_events

        with patch.dict(os.environ, {}, clear=True):
            buf = io.StringIO()
            with redirect_stderr(buf):
                vp_worker_events.emit("status", state="ready")
            self.assertEqual(buf.getvalue(), "")

    def test_worker_events_emit_compact_json_on_stderr(self):
        import vp_worker_events

        with patch.dict(os.environ, {"VOICEPI_WORKER_EVENTS": "1"}, clear=True):
            buf = io.StringIO()
            with redirect_stderr(buf):
                vp_worker_events.emit("status", state="ready", model="large-v3")
            line = buf.getvalue().strip()

        self.assertTrue(line.startswith("[worker-event] "))
        payload = json.loads(line.removeprefix("[worker-event] "))
        self.assertEqual(payload, {
            "event": "status",
            "state": "ready",
            "model": "large-v3",
        })


class DeviceResolutionTests(unittest.TestCase):
    def setUp(self):
        self._old_compute = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old_compute is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old_compute

    def test_auto_uses_cuda_when_available(self):
        voice_pi = load_voice_pi(cuda_devices=1)

        self.assertEqual(
            voice_pi._resolve_device("auto"),
            ("cuda", "int8_float16"),
        )

    def test_auto_falls_back_to_cpu_without_cuda(self):
        voice_pi = load_voice_pi(cuda_devices=0)

        self.assertEqual(voice_pi._resolve_device("auto"), ("cpu", "int8"))

    def test_explicit_cpu_and_cuda(self):
        voice_pi = load_voice_pi()

        self.assertEqual(voice_pi._resolve_device("cpu"), ("cpu", "int8"))
        self.assertEqual(
            voice_pi._resolve_device("cuda"),
            ("cuda", "int8_float16"),
        )

    def test_invalid_device_is_rejected(self):
        voice_pi = load_voice_pi()

        with self.assertRaises(ValueError):
            voice_pi._resolve_device("cdua")


class ComputeTypeOverrideTests(unittest.TestCase):
    """VOICEPI_COMPUTE_TYPE overrides the auto-picked compute_type for
    cuda / cpu / auto-on-gpu / auto-on-cpu — and an unset/empty env leaves
    the int8_float16-on-GPU / int8-on-CPU defaults untouched."""

    def setUp(self):
        self._old = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old

    def test_override_applies_to_explicit_cuda(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "float16"))

    def test_override_applies_to_explicit_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi()
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "float32"))

    def test_override_applies_to_auto_on_gpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "bfloat16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cuda", "bfloat16"))

    def test_override_applies_to_auto_on_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi(cuda_devices=0)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cpu", "float32"))

    def test_empty_env_leaves_defaults_untouched(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "   "  # whitespace only
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))

    def test_default_unchanged_when_env_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))


class DebugConfigTests(unittest.TestCase):
    """VOICEPI_DEBUG triggers a startup dump of every effective setting
    + the env-var source annotation — so users can verify their setx
    actually arrived in the running process."""

    def setUp(self):
        # Cache + clear env we mutate so the dump is deterministic
        self._cached = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_COMPUTE_TYPE", "VOICEPI_INITIAL_PROMPT",
            "VOICEPI_BEAM_SIZE", "VOICEPI_QUIT_COUNT",
            "VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT",
            "VOICEPI_LANG", "VOICEPI_MODEL", "VOICEPI_DEVICE",
            "VOICEPI_KEY", "VOICEPI_INJECT_MODE",
            "VOICEPI_DICTIONARY", "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_STT_BACKEND",
        )}

    def tearDown(self):
        for k, v in self._cached.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v

    def _args(self, **over):
        defaults = dict(key="ctrl_r", model="large-v3", lang="da",
                        autodetect=False, device="cuda", mode="type")
        defaults.update(over)
        return types.SimpleNamespace(**defaults)

    def test_dump_includes_all_expected_sections(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        os.environ["VOICEPI_INITIAL_PROMPT"] = "foo,bar,baz,qux"
        os.environ["VOICEPI_BEAM_SIZE"] = "8"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()

        # header + every row label appears
        self.assertIn("[debug] effective settings:", out)
        for label in ("--key", "--model", "--lang", "--device",
                      "stt backend", "compute_type", "beam_size", "initial_prompt",
                      "dictionary", "quit", "audio thresholds", "XKB (Wayland)",
                      "inject mode"):
            self.assertIn(label, out)

        # env-sourced values are surfaced + annotated with the env var name
        self.assertIn("VOICEPI_COMPUTE_TYPE=float16", out)
        self.assertIn("VOICEPI_BEAM_SIZE=8", out)
        self.assertIn("VOICEPI_KEY=(unset)", out)
        self.assertIn("VOICEPI_INJECT_MODE=(unset)", out)
        self.assertIn("large-v3", out)
        self.assertIn("float16", out)
        # prompt is shown with its length + a preview substring
        self.assertIn("15 chars", out)
        self.assertIn("foo,bar,baz,qux", out)

    def test_long_prompt_is_truncated(self):
        os.environ["VOICEPI_INITIAL_PROMPT"] = "x" * 200
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()
        self.assertIn("200 chars", out)
        self.assertIn("...", out)  # truncated marker
        # full 200-char string is NOT in the output
        self.assertNotIn("x" * 200, out)

    def test_unset_env_shows_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "int8_float16"),
        out = buf.getvalue()
        self.assertIn("VOICEPI_COMPUTE_TYPE=(unset)", out)
        self.assertIn("VOICEPI_INITIAL_PROMPT", out)  # row exists
        self.assertIn("(unset)", out)  # prompt shows (unset) too

    def test_autodetect_flag_overrides_lang_in_display(self):
        os.environ["VOICEPI_LANG"] = "da"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(
                self._args(lang="da", autodetect=True), "cuda", "float16")
        out = buf.getvalue()
        # final resolved lang is 'auto' even though VOICEPI_LANG=da
        # because --autodetect was passed
        self.assertRegex(out, r"--lang\s+auto\b")
        self.assertIn("--autodetect=True", out)


class ArgumentParserTests(unittest.TestCase):
    def test_parser_rejects_invalid_device(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        with redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                parser.parse_args(["--device", "cdua"])

    def test_parser_accepts_supported_devices(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        for device in voice_pi.VALID_DEVICES:
            with self.subTest(device=device):
                self.assertEqual(
                    parser.parse_args(["--device", device]).device,
                    device,
                )

    def test_parser_uses_key_env_default(self):
        with _env(VOICEPI_KEY="ctrl_l+space"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).key, "ctrl_l+space")
            self.assertEqual(parser.parse_args(["--key", "f9"]).key, "f9")

    def test_parser_uses_inject_mode_env_default(self):
        with _env(VOICEPI_INJECT_MODE="paste"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).mode, "paste")
            self.assertEqual(parser.parse_args(["--no-type"]).mode, "print")
            self.assertEqual(parser.parse_args(["--paste"]).mode, "paste")
            self.assertEqual(parser.parse_args(["--type"]).mode, "type")

    def test_parser_defaults_to_auto_inject_mode(self):
        old = os.environ.pop("VOICEPI_INJECT_MODE", None)
        try:
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()
        finally:
            if old is not None:
                os.environ["VOICEPI_INJECT_MODE"] = old

        self.assertEqual(parser.parse_args([]).mode, "auto")

    def test_parser_accepts_json_doctor_and_model_capacity(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        ns = parser.parse_args(["--json", "--doctor", "--model-capacity"])

        self.assertTrue(ns.json)
        self.assertTrue(ns.doctor)
        self.assertTrue(ns.model_capacity)

    def test_dictionary_status_exits_from_parser(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            with _env(VOICEPI_DICTIONARY=path):
                voice_pi = load_voice_pi()
                parser = voice_pi.build_arg_parser()

                with _capture_stdout() as buf:
                    with self.assertRaises(SystemExit) as cm:
                        parser.parse_args(["--dictionary-status"])

        self.assertEqual(cm.exception.code, 0)
        self.assertIn("managed path:", buf.getvalue())

    def test_dictionary_add_exits_from_parser_and_writes_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            with _env(VOICEPI_DICTIONARY=path):
                voice_pi = load_voice_pi()
                parser = voice_pi.build_arg_parser()

                with _capture_stdout():
                    with self.assertRaises(SystemExit) as cm:
                        parser.parse_args(["--dictionary-add", "OpenClaw"])

                with open(path, encoding="utf-8") as f:
                    data = json.load(f)

        self.assertEqual(cm.exception.code, 0)
        self.assertEqual(data["terms"], ["OpenClaw"])


class ModelCapacityTests(unittest.TestCase):
    def test_estimate_model_fits_uses_free_and_total_vram(self):
        import vp_model_capacity

        gpus = [vp_model_capacity.GpuInfo(
            index=0,
            name="RTX Test",
            total_mb=8192,
            free_mb=4096,
        )]

        _, fits = vp_model_capacity.estimate_model_fits(gpus)
        by_name = {fit.profile.name: fit for fit in fits}

        self.assertEqual(by_name["Whisper large-v3-turbo"].status, "ok")
        self.assertEqual(by_name["Whisper large-v3 float16"].status, "free-vram")
        self.assertEqual(by_name["NVIDIA Parakeet TDT 1.1B"].status, "too-small")

    def test_capacity_report_json_shape(self):
        import vp_model_capacity

        with patch("vp_model_capacity.query_gpus", return_value=[
            vp_model_capacity.GpuInfo(0, "RTX Test", 16384, 12000),
        ]):
            data = json.loads(vp_model_capacity.capacity_report(as_json=True))

        self.assertEqual(data["gpus"][0]["name"], "RTX Test")
        self.assertTrue(any(item["name"] == "Whisper large-v3 float16"
                            for item in data["models"]))

    def test_capacity_report_plain_mentions_free_vram(self):
        import vp_model_capacity

        with patch("vp_model_capacity.query_gpus", return_value=[
            vp_model_capacity.GpuInfo(0, "RTX Test", 8192, 4096),
        ]):
            report = vp_model_capacity.capacity_report()

        self.assertIn("4096 MB free / 8192 MB total", report)
        self.assertIn("Whisper large-v3-turbo", report)
        self.assertIn("Use free VRAM", report)


class ExternalApiTests(unittest.TestCase):
    def test_external_api_import_path_does_not_require_numpy_until_transcription(self):
        completed = subprocess.run(
            [sys.executable, "-S", "-c",
             "import vp_external_api, vp_postprocess; "
             "assert vp_external_api.DEFAULT_OPENAI_BASE_URL; "
             "assert 'openai' in vp_postprocess.VALID_PROCESSORS"],
            cwd=os.getcwd(),
            capture_output=True,
            text=True,
            timeout=10,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_external_stt_maps_local_whisper_default_to_openai_model(self):
        with _env(VOICEPI_MODEL="large-v3-turbo", VOICEPI_STT_API_KEY="test-key"):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3-turbo")

        self.assertEqual(settings.model, "gpt-4o-mini-transcribe")

    def test_external_stt_configured_model_takes_precedence(self):
        with _env(VOICEPI_STT_MODEL="gpt-4o-transcribe", VOICEPI_STT_API_KEY="test-key"):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.model, "gpt-4o-transcribe")

    def test_groq_base_url_accepts_groq_api_key_alias(self):
        with _env(
            VOICEPI_STT_API_KEY=None,
            OPENAI_API_KEY=None,
            GROQ_API_KEY="groq-key",
            VOICEPI_STT_BASE_URL="https://api.groq.com/openai/v1",
            VOICEPI_STT_MODEL="whisper-large-v3-turbo",
        ):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.base_url, "https://api.groq.com/openai/v1")
        self.assertEqual(settings.model, "whisper-large-v3-turbo")
        self.assertEqual(settings.api_key, "groq-key")

    def test_non_groq_base_url_does_not_use_groq_api_key_alias(self):
        with _env(
            VOICEPI_STT_API_KEY=None,
            OPENAI_API_KEY=None,
            GROQ_API_KEY="groq-key",
            VOICEPI_STT_BASE_URL="https://api.example.test/v1",
            VOICEPI_STT_MODEL="custom-transcribe",
        ):
            sys.modules.pop("vp_external_api", None)
            import vp_external_api

            settings = vp_external_api.load_stt_api_settings("large-v3")

        self.assertEqual(settings.api_key, "")

    def test_external_transcription_posts_multipart_audio(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer
        sys.modules.pop("vp_external_api", None)
        np = getattr(AudioDspTests, "np", None)
        if np is None:
            import numpy as np
        sys.modules["numpy"] = np

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["auth"] = self.headers.get("Authorization")
                calls["content_type"] = self.headers.get("Content-Type")
                calls["body"] = body
                data = json.dumps({"text": "Hello Codex", "language": "en"}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        with _env(
            VOICEPI_STT_API_KEY="test-key",
            VOICEPI_STT_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
        ):
            import vp_external_api

            model = vp_external_api.ExternalTranscriptionModel("gpt-4o-mini-transcribe")
            segments, info = model.transcribe(np.zeros(1600, dtype=np.float32), language="en")

        self.assertEqual(calls["path"], "/v1/audio/transcriptions")
        self.assertEqual(calls["auth"], "Bearer test-key")
        self.assertIn("multipart/form-data", calls["content_type"])
        self.assertIn(b'gpt-4o-mini-transcribe', calls["body"])
        self.assertIn(b"audio.wav", calls["body"])
        self.assertEqual(segments[0].text.strip(), "Hello Codex")
        self.assertEqual(info.language, "en")


class InjectStrategyTests(unittest.TestCase):
    def setUp(self):
        for n in ("vp_inject", "vp_keymap"):
            sys.modules.pop(n, None)
        import vp_inject
        self.inject = vp_inject

    def _dummy(self, title=None, process=None):
        return types.SimpleNamespace(
            _inject_target_title=title,
            _inject_target_process=process,
        )

    def test_windows_terminal_targets_prefer_paste(self):
        target = self._dummy("Administrator: Windows PowerShell", "WindowsTerminal.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertTrue(
                self.inject.InjectMixin._target_prefers_paste(target))

    def test_regular_windows_targets_still_type(self):
        target = self._dummy("Untitled - Notepad", "notepad.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertFalse(
                self.inject.InjectMixin._target_prefers_paste(target))

    def test_windows_layout_sensitive_text_prefers_paste(self):
        target = self._dummy("Untitled - Notepad", "notepad.exe")

        with patch.object(self.inject.os, "name", "nt"):
            self.assertTrue(
                self.inject.InjectMixin._text_prefers_paste(target, "I'm testing"))
            self.assertTrue(
                self.inject.InjectMixin._text_prefers_paste(target, 'say "hello"'))
            self.assertFalse(
                self.inject.InjectMixin._text_prefers_paste(target, "plain ascii"))

    def test_windows_auto_pastes_layout_sensitive_text(self):
        with open("vp_inject.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("_WINDOWS_LAYOUT_SENSITIVE_CHARS", script)
        self.assertIn("self._text_prefers_paste(text)", script)

    def test_non_windows_targets_still_type(self):
        target = self._dummy("Windows Terminal", "WindowsTerminal.exe")

        with patch.object(self.inject.os, "name", "posix"):
            self.assertFalse(
                self.inject.InjectMixin._target_prefers_paste(target))


@contextmanager
def _env(**kwargs):
    old = {k: os.environ.get(k) for k in kwargs}
    for k, v in kwargs.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    try:
        yield
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class BuildYdotoolOpsTests(unittest.TestCase):
    """_build_ydotool_ops: tekst → liste af (subkommando, *args) tupler."""

    def setUp(self):
        self.vp = load_voice_pi()
        self.dk = self.vp._LAYOUT_KEYCODES['dk']

    def test_ascii_only_is_single_type_op(self):
        ops = self.vp._build_ydotool_ops("hello", {})
        self.assertEqual(ops, [('type', '--', 'hello')])

    def test_empty_string_gives_no_ops(self):
        ops = self.vp._build_ydotool_ops("", {})
        self.assertEqual(ops, [])

    def test_oe_splits_into_key_op(self):
        ops = self.vp._build_ydotool_ops("ø", self.dk)
        self.assertEqual(ops, [('key', '40:1', '40:0')])

    def test_mixed_flushes_ascii_buffer_before_special(self):
        # "høre" → type "h", key ø, type "re"
        ops = self.vp._build_ydotool_ops("høre", self.dk)
        self.assertEqual(ops, [
            ('type', '--', 'h'),
            ('key', '40:1', '40:0'),
            ('type', '--', 're'),
        ])

    def test_question_mark_uses_nordic_keycode(self):
        # '?' er shift+KEY_MINUS i nordiske layouts, ikke shift+KEY_SLASH
        ops = self.vp._build_ydotool_ops("hvad?", self.dk)
        self.assertEqual(ops, [
            ('type', '--', 'hvad'),
            ('key', '42:1', '12:1', '12:0', '42:0'),
        ])

    def test_consecutive_special_chars_each_get_key_op(self):
        ops = self.vp._build_ydotool_ops("æøå", self.dk)
        self.assertEqual(ops, [
            ('key', '39:1', '39:0'),  # æ
            ('key', '40:1', '40:0'),  # ø
            ('key', '26:1', '26:0'),  # å
        ])

    def test_uppercase_special_char(self):
        ops = self.vp._build_ydotool_ops("Ø", self.dk)
        self.assertEqual(ops, [('key', '42:1', '40:1', '40:0', '42:0')])

    def test_ascii_after_special_is_flushed(self):
        ops = self.vp._build_ydotool_ops("åben", self.dk)
        self.assertEqual(ops, [
            ('key', '26:1', '26:0'),  # å
            ('type', '--', 'ben'),
        ])

    def test_no_map_passthrough(self):
        # Uden keycode_map (f.eks. us-layout) → alt sendes som type
        ops = self.vp._build_ydotool_ops("høre", {})
        self.assertEqual(ops, [('type', '--', 'høre')])


class LayoutKeycodeMapTests(unittest.TestCase):
    """Mapningens indhold: hvert layout har de forventede specialtegn."""

    def setUp(self):
        self.vp = load_voice_pi()

    def _assert_has_chars(self, layout: str, chars: str):
        m = self.vp._LAYOUT_KEYCODES[layout]
        for ch in chars:
            with self.subTest(layout=layout, char=ch):
                self.assertIn(ch, m)

    def test_dk_has_ae_oe_aa(self):
        self._assert_has_chars('dk', 'æøåÆØÅ')

    def test_no_aliases_dk(self):
        self.assertIs(
            self.vp._LAYOUT_KEYCODES['no'],
            self.vp._LAYOUT_KEYCODES['dk'],
        )

    def test_se_has_ae_oe_aa(self):
        self._assert_has_chars('se', 'äöåÄÖÅ')

    def test_de_has_umlauts(self):
        self._assert_has_chars('de', 'äöüÄÖÜ')

    def test_fi_has_ae_oe(self):
        self._assert_has_chars('fi', 'äöÄÖ')

    def test_all_layouts_have_nordic_punct(self):
        punct = '?-_:;/"'
        for layout in ('dk', 'no', 'se', 'de', 'fi'):
            self._assert_has_chars(layout, punct)

    def test_es_has_n_tilde_and_accented_vowels(self):
        self._assert_has_chars('es', 'ñÑáéíóúÁÉÍÓÚüÜ')

    def test_pt_has_cedilla_and_accented_vowels(self):
        self._assert_has_chars('pt', 'çÇáéíóúÁÉÍÓÚàÀãõÃÕâêôÂÊÔ')

    def test_br_has_cedilla_tilde_circumflex(self):
        self._assert_has_chars('br', 'çÇãõÃÕâêôÂÊÔáéíóúÁÉÍÓÚ')

    def test_pl_has_polish_chars(self):
        self._assert_has_chars('pl', 'ąęóśźżćńłĄĘÓŚŹŻĆŃŁ')

    def test_ua_has_full_cyrillic_alphabet(self):
        self._assert_has_chars('ua', 'йцукенгшщзхїфівапролджєґячсмитьбюЙЦУКЕНГШЩЗХЇФІВАПРОЛДЖЄҐЯЧСМИТЬБЮ')

    def test_ru_not_in_lang_to_xkb(self):
        self.assertNotIn('ru', self.vp._LANG_TO_XKB)

    def test_uk_maps_to_ua(self):
        self.assertEqual(self.vp._LANG_TO_XKB.get('uk'), 'ua')

    def test_keycodes_are_balanced_per_key(self):
        # Stærkere end "lige antal codes": hvert keycode skal have lige
        # mange press (N:1) og release (N:0) i en sekvens — ellers hænger
        # fx Shift(42)/AltGr(100) og korrumperer efterfølgende input.
        import collections
        for layout, m in self.vp._LAYOUT_KEYCODES.items():
            for ch, codes in m.items():
                with self.subTest(layout=layout, char=ch):
                    bal: "collections.Counter[str]" = collections.Counter()
                    for tok in codes:
                        key, sep, state = tok.partition(":")
                        self.assertTrue(sep and state in ("0", "1"),
                                        f"Ugyldig token {tok!r} for '{ch}'")
                        bal[key] += 1 if state == "1" else -1
                    for key, net in bal.items():
                        self.assertEqual(
                            net, 0,
                            f"Keycode {key} ubalanceret for '{ch}' i "
                            f"layout '{layout}' (net={net} press-release)")


class DetectXkbLayoutTests(unittest.TestCase):
    """_detect_xkb_layout: prioritetsrækkefølge og fallback."""

    def setUp(self):
        self.vp = load_voice_pi()
        # Ryd env-variabler der ellers forstyrrer
        self._patches = [
            patch.dict(os.environ, {}, clear=False),
        ]
        for p in self._patches:
            p.start()
        os.environ.pop('VOICEPI_XKB_LAYOUT', None)
        os.environ.pop('XKB_DEFAULT_LAYOUT', None)

    def tearDown(self):
        for p in self._patches:
            p.stop()

    def test_voicepi_env_var_takes_priority(self):
        with _env(VOICEPI_XKB_LAYOUT='se', XKB_DEFAULT_LAYOUT='de'):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'se')

    def test_xkb_default_layout_beats_keyboard_file(self):
        with _env(XKB_DEFAULT_LAYOUT='de'):
            with patch('builtins.open', side_effect=FileNotFoundError):
                result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'de')

    def test_keyboard_file_parsed_correctly(self):
        content = 'XKBLAYOUT="dk"\nXKBVARIANT=""\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout(None)
        self.assertEqual(result, 'dk')

    def test_us_layout_in_keyboard_file_is_ignored(self):
        content = 'XKBLAYOUT="us"\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout('da')
        # Falder igennem til lang-hint: da → dk
        self.assertEqual(result, 'dk')

    def test_lang_hint_da_gives_dk(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'dk')

    def test_lang_hint_nb_gives_no(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('nb')
        self.assertEqual(result, 'no')

    def test_no_hints_returns_none(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout(None)
        self.assertIsNone(result)


class ModuleSurfaceTests(unittest.TestCase):
    """voice_pi.py re-exports names that were moved into vp_cli / vp_transcribe
    when the file was split. Tests, the installer, and downstream callers
    still reach for these on the voice_pi module — make sure they resolve."""

    def test_voice_pi_reexports_cli_symbols(self):
        vp = load_voice_pi()
        for name in ("build_arg_parser", "_print_effective_config",
                     "KEY", "MODEL_NAME", "DEVICE", "LANG",
                     "INJECT_MODE", "VALID_INJECT_MODES",
                     "QUIT_COUNT", "QUIT_WINDOW_MS", "BEAM_SIZE"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")

    def test_voice_pi_reexports_transcribe_symbols(self):
        vp = load_voice_pi()
        for name in ("_transcribe", "_HALLUCINATIONS",
                     "is_hallucination", "SR", "INITIAL_PROMPT",
                     "TEMPERATURES", "CONTEXT_MIN_SECONDS",
                     "STT_BACKEND", "VALID_STT_BACKENDS",
                     "load_stt_model"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")

    def test_voice_pi_reexports_device_audio_keymap(self):
        vp = load_voice_pi()
        for name in ("_resolve_device", "VALID_DEVICES",
                     "_noise_snr", "_boost_quiet", "_looks_like_speech",
                     "TARGET_DBFS",
                     "_LAYOUT_KEYCODES", "_LANG_TO_XKB",
                     "_detect_xkb_layout", "_build_ydotool_ops"):
            self.assertTrue(hasattr(vp, name),
                            f"voice_pi.{name} missing — re-export broken")


class HallucinationFilterTests(unittest.TestCase):
    """is_hallucination filters Whisper's known output when fed near-silence."""

    def setUp(self):
        # Pure import — no numpy / faster_whisper needed for this surface.
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        import vp_transcribe
        self.t = vp_transcribe

    def test_known_hallucination_filtered(self):
        for phrase in ("tak", "Tak.", "TAK FORDI DU SÅ MED",
                       "thank you for watching", "Undertekster af"):
            self.assertTrue(self.t.is_hallucination(phrase),
                            f"{phrase!r} should match")

    def test_trailing_whitespace_still_matches(self):
        self.assertTrue(self.t.is_hallucination("tak.  \n"))

    def test_genuine_text_not_filtered(self):
        for phrase in ("hello world", "tak for hjælpen",
                       "dette er en sætning der ikke er hallucination"):
            self.assertFalse(self.t.is_hallucination(phrase),
                             f"{phrase!r} should NOT match")


class CliModuleIsolationTests(unittest.TestCase):
    """vp_cli.build_arg_parser must work standalone — no voice_pi import.
    Catches regressions where someone accidentally re-couples them."""

    def setUp(self):
        # vp_cli depends only on vp_audio, vp_device, vp_transcribe — all
        # of which need numpy. Stub it the same way load_voice_pi does so
        # this test runs even without numpy installed.
        for n in ("voice_pi", "vp_cli", "vp_transcribe",
                  "vp_audio", "vp_device"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))

    def test_parser_works_without_voice_pi(self):
        before = set(sys.modules)
        import vp_cli
        ns = vp_cli.build_arg_parser().parse_args([])
        # Defaults pulled from env vars; just check the shape.
        for attr in ("key", "model", "lang", "device", "mode", "autodetect"):
            self.assertTrue(hasattr(ns, attr),
                            f"parser missing --{attr}")
        # voice_pi may already have been loaded by an earlier test, but
        # importing vp_cli here must NOT pull it in fresh.
        newly_loaded = set(sys.modules) - before
        self.assertNotIn("voice_pi", newly_loaded,
                         "vp_cli must not pull in voice_pi")


class TemperatureParseTests(unittest.TestCase):
    """vp_transcribe._parse_temperatures: CSV float list with a safe
    default if unset, empty, or malformed."""

    def setUp(self):
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules.setdefault("numpy", types.ModuleType("numpy"))
        import vp_transcribe
        self.t = vp_transcribe

    def test_unset_returns_default_ladder(self):
        self.assertEqual(self.t._parse_temperatures(None), [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures(""), [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures("   "), [0.0, 0.2])

    def test_single_value_locks_decode(self):
        self.assertEqual(self.t._parse_temperatures("0.0"), [0.0])
        self.assertEqual(self.t._parse_temperatures("0"), [0.0])
        self.assertEqual(self.t._parse_temperatures("0.4"), [0.4])

    def test_csv_ladder(self):
        self.assertEqual(self.t._parse_temperatures("0.0,0.2,0.4"),
                         [0.0, 0.2, 0.4])
        # Whitespace tolerated around commas.
        self.assertEqual(self.t._parse_temperatures(" 0.0 , 0.5 "),
                         [0.0, 0.5])

    def test_malformed_falls_back_to_default(self):
        self.assertEqual(self.t._parse_temperatures("not-a-number"),
                         [0.0, 0.2])
        self.assertEqual(self.t._parse_temperatures("0.0,abc"),
                         [0.0, 0.2])


class ContextMinSecondsTests(unittest.TestCase):
    """VOICEPI_CONTEXT_MIN_SECONDS gates condition_on_previous_text:
      * 0 (default)  -> always False (backwards-compatible)
      * > 0          -> True only when utterance duration meets the bar
    The bar lives in vp_transcribe.CONTEXT_MIN_SECONDS; the gate itself
    is one line in _transcribe so we mirror its expression here."""

    def _gate(self, threshold: float, dur: float) -> bool:
        return threshold > 0 and dur >= threshold

    def test_zero_threshold_never_enables_context(self):
        for dur in (0.0, 1.0, 5.0, 30.0, 1000.0):
            self.assertFalse(self._gate(0.0, dur),
                             f"threshold=0, dur={dur} must stay False")

    def test_positive_threshold_gates_on_duration(self):
        self.assertFalse(self._gate(5.0, 4.9))
        self.assertTrue(self._gate(5.0, 5.0))
        self.assertTrue(self._gate(5.0, 19.4))


class MetricsTests(unittest.TestCase):
    def test_append_jsonl_writes_unicode_event(self):
        for n in ("vp_metrics",):
            sys.modules.pop(n, None)
        import vp_metrics

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            vp_metrics.append_jsonl(path, {"text": "rødgrød", "n": 1})
            with open(path, encoding="utf-8") as f:
                data = f.read()
            self.assertIn('"text": "rødgrød"', data)
            self.assertIn('"n": 1', data)
        finally:
            try:
                os.remove(path)
            except OSError:
                pass


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_CONFIG", "VOICEPI_MODEL", "VOICEPI_LANG",
        )}
        for n in ("vp_config",):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k, v in self._old.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v
        sys.modules.pop("vp_config", None)

    def test_config_value_beats_env_and_persists(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "config.json")
            os.environ["VOICEPI_CONFIG"] = path
            os.environ["VOICEPI_LANG"] = "en"
            import vp_config

            vp_config.save_config({"lang": "da", "model": "large-v3"})
            self.assertEqual(vp_config.get_value("VOICEPI_LANG"), "da")
            self.assertEqual(vp_config.get_value("VOICEPI_MODEL"), "large-v3")
            self.assertEqual(vp_config.apply_config_to_environ(), {"VOICEPI_LANG", "VOICEPI_MODEL"})
            self.assertEqual(os.environ["VOICEPI_LANG"], "da")

class TranscribeDetailTests(unittest.TestCase):
    def setUp(self):
        if not hasattr(AudioDspTests, "np"):
            raise unittest.SkipTest("real numpy unavailable")
        for n in ("vp_transcribe", "vp_audio"):
            sys.modules.pop(n, None)
        sys.modules["numpy"] = AudioDspTests.np
        import vp_transcribe
        self.t = vp_transcribe
        self.np = AudioDspTests.np

    def test_transcribe_detail_collects_metadata_and_vad_settings(self):
        np = self.np

        class Segment:
            text = " hej"
            start = 0.0
            end = 1.0
            avg_logprob = -0.1
            no_speech_prob = 0.02
            compression_ratio = 1.1

        class Info:
            language = "da"
            language_probability = 0.98

        class Model:
            def __init__(self):
                self.kwargs = None

            def transcribe(self, audio, **kwargs):
                self.kwargs = kwargs
                return [Segment()], Info()

        audio = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(40)
        ]).reshape(-1, 1)
        pcm = (audio * 32767).astype(np.int16)
        model = Model()

        with _capture_stdout():
            result = self.t._transcribe_detail(model, pcm, "da")

        self.assertEqual(result.text, "hej")
        self.assertEqual(result.language, "da")
        self.assertEqual(result.language_probability, 0.98)
        self.assertGreaterEqual(result.compute_s, 0)
        self.assertIsNotNone(result.real_time_factor)
        self.assertEqual(result.segments[0]["avg_logprob"], -0.1)
        self.assertEqual(
            model.kwargs["vad_parameters"]["threshold"],
            self.t.VAD_THRESHOLD,
        )


class STTBackendTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_STT_BACKEND", "VOICEPI_MODEL", "VOICEPI_PARAKEET_MODEL",
            "VOICEPI_STT_BASE_URL", "VOICEPI_STT_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
        for n in list(sys.modules):
            if (n in ("vp_transcribe", "vp_audio", "vp_parakeet",
                      "faster_whisper", "nemo")
                    or n.startswith("nemo.")):
                sys.modules.pop(n, None)

    def tearDown(self):
        for k, v in self._old.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v
        for n in list(sys.modules):
            if n in ("vp_transcribe", "vp_parakeet") or n.startswith("nemo."):
                sys.modules.pop(n, None)

    def test_default_backend_loads_faster_whisper_without_nemo(self):
        created = {}
        fw = types.ModuleType("faster_whisper")

        class WhisperModel:
            def __init__(self, model_name, *, device, compute_type):
                created["args"] = (model_name, device, compute_type)

        fw.WhisperModel = WhisperModel
        sys.modules["faster_whisper"] = fw
        sys.modules["numpy"] = getattr(AudioDspTests, "np", types.ModuleType("numpy"))

        import vp_transcribe

        model = vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

        self.assertIsInstance(model, WhisperModel)
        self.assertEqual(created["args"], ("large-v3-turbo", "cpu", "int8"))
        self.assertNotIn("nemo.collections.asr", sys.modules)

    def test_invalid_backend_is_rejected(self):
        os.environ["VOICEPI_STT_BACKEND"] = "bogus"
        sys.modules["numpy"] = getattr(AudioDspTests, "np", types.ModuleType("numpy"))
        import vp_transcribe

        with self.assertRaisesRegex(ValueError, "VOICEPI_STT_BACKEND"):
            vp_transcribe.load_stt_model("large-v3-turbo", "cpu", "int8")

    def test_parakeet_missing_deps_error_is_actionable(self):
        os.environ["VOICEPI_STT_BACKEND"] = "parakeet"
        sys.modules["numpy"] = getattr(AudioDspTests, "np", types.ModuleType("numpy"))
        import vp_transcribe

        real_import = __import__

        def fake_import(name, *args, **kwargs):
            if name == "nemo.collections.asr" or name.startswith("nemo"):
                raise ImportError("no nemo")
            return real_import(name, *args, **kwargs)

        with patch("builtins.__import__", side_effect=fake_import):
            with self.assertRaisesRegex(RuntimeError, "requirements-parakeet.txt"):
                vp_transcribe.load_stt_model("large-v3-turbo", "cuda", "float16")

    def test_openai_backend_uses_external_transcription_adapter(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_STT_API_KEY"] = "test-key"
        import vp_transcribe
        import vp_external_api

        with patch.object(vp_external_api.ExternalTranscriptionModel, "__init__", return_value=None) as init:
            model = vp_transcribe.load_stt_model("gpt-4o-mini-transcribe", "cpu", "int8")

        self.assertIsInstance(model, vp_external_api.ExternalTranscriptionModel)
        init.assert_called_once_with("gpt-4o-mini-transcribe")

    def test_local_only_blocks_openai_stt_backend(self):
        os.environ["VOICEPI_STT_BACKEND"] = "openai"
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_transcribe

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_transcribe.load_stt_model("gpt-4o-mini-transcribe", "cpu", "int8")

    def test_parakeet_adapter_uses_nemo_stub_and_default_model(self):
        calls = {}

        fake_np = types.ModuleType("numpy")
        fake_np.float32 = object()
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: True)
        sys.modules["torch"] = torch

        class FakeNemoModel:
            def to(self, device):
                calls["device"] = device

            def eval(self):
                calls["eval"] = True

            def freeze(self):
                calls["freeze"] = True

            def transcribe(self, paths, batch_size=1):
                calls["path"] = paths[0]
                calls["path_exists_during_call"] = os.path.exists(paths[0])
                calls["batch_size"] = batch_size
                return [" hello"]

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                calls["model_name"] = model_name
                return FakeNemoModel()

        nemo = types.ModuleType("nemo")
        collections = types.ModuleType("nemo.collections")
        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        collections.asr = asr
        nemo.collections = collections
        sys.modules["nemo"] = nemo
        sys.modules["nemo.collections"] = collections
        sys.modules["nemo.collections.asr"] = asr

        import vp_parakeet
        model = vp_parakeet.ParakeetModel("large-v3-turbo", device="cuda")
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name

        class FakeAudio:
            def reshape(self, *_args):
                return self

            def astype(self, *_args):
                return self

        with patch.object(vp_parakeet, "_write_wav", return_value=path):
            segments, info = model.transcribe(FakeAudio())

        self.assertEqual(
            calls["model_name"], "nvidia/parakeet-tdt-0.6b-v3")
        self.assertEqual(calls["device"], "cuda")
        self.assertTrue(calls["eval"])
        self.assertTrue(calls["freeze"])
        self.assertTrue(calls["path_exists_during_call"])
        self.assertFalse(os.path.exists(calls["path"]))
        self.assertEqual(calls["batch_size"], 1)
        self.assertEqual(segments[0].text, "hello")
        self.assertIsNone(info.language)

    def test_parakeet_ignores_whisper_model_names_without_explicit_override(self):
        calls = {}
        fake_np = types.ModuleType("numpy")
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: True)
        sys.modules["torch"] = torch

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                calls["model_name"] = model_name
                return types.SimpleNamespace()

        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        sys.modules["nemo"] = types.ModuleType("nemo")
        sys.modules["nemo.collections"] = types.ModuleType("nemo.collections")
        sys.modules["nemo.collections.asr"] = asr

        import vp_parakeet

        vp_parakeet.ParakeetModel("large-v3", device="cuda")

        self.assertEqual(
            calls["model_name"], "nvidia/parakeet-tdt-0.6b-v3")

    def test_parakeet_cuda_requires_cuda_enabled_torch(self):
        fake_np = types.ModuleType("numpy")
        sys.modules["numpy"] = fake_np
        torch = types.ModuleType("torch")
        torch.cuda = types.SimpleNamespace(is_available=lambda: False)
        sys.modules["torch"] = torch

        class ASRModel:
            @staticmethod
            def from_pretrained(model_name):
                return types.SimpleNamespace()

        asr = types.ModuleType("nemo.collections.asr")
        asr.models = types.SimpleNamespace(ASRModel=ASRModel)
        sys.modules["nemo"] = types.ModuleType("nemo")
        sys.modules["nemo.collections"] = types.ModuleType("nemo.collections")
        sys.modules["nemo.collections.asr"] = asr

        import vp_parakeet

        with self.assertRaisesRegex(RuntimeError, "CUDA-enabled PyTorch"):
            vp_parakeet.ParakeetModel("large-v3", device="cuda")

    def test_parakeet_accepts_explicit_nvidia_model_name(self):
        import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("nvidia/custom-parakeet"),
            "nvidia/custom-parakeet",
        )

    def test_parakeet_env_override_wins_over_whisper_model_name(self):
        os.environ["VOICEPI_PARAKEET_MODEL"] = "nvidia/explicit-parakeet"
        import vp_parakeet

        self.assertEqual(
            vp_parakeet.resolve_parakeet_model_name("large-v3"),
            "nvidia/explicit-parakeet",
        )

    def test_parakeet_model_dropdown_options_are_exported(self):
        import vp_parakeet

        self.assertEqual(vp_parakeet.PARAKEET_MODELS[0], vp_parakeet.DEFAULT_MODEL)
        self.assertEqual(vp_parakeet.PARAKEET_MODELS, [
            "nvidia/parakeet-tdt-0.6b-v3",
            "nvidia/parakeet-tdt-1.1b",
            "nvidia/parakeet-tdt-0.6b-v2",
        ])

    def test_parakeet_suppresses_irrelevant_pydub_ffmpeg_warning(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("warnings.filterwarnings", script)
        self.assertIn("Couldn't find ffmpeg or avconv", script)

    def test_parakeet_quiets_nemo_output_unless_stt_debug_is_enabled(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        self.assertIn("def _nemo_output_context", script)
        self.assertIn('os.environ.get("VOICEPI_STT_DEBUG")', script)
        self.assertIn("contextlib.redirect_stdout", script)
        self.assertIn("contextlib.redirect_stderr", script)
        self.assertIn("with _nemo_output_context():", script)

    def test_parakeet_model_load_and_transcribe_are_quieted(self):
        import vp_parakeet

        with open(vp_parakeet.__file__, encoding="utf-8") as f:
            script = f.read()
        load = script.index("self._model = nemo_asr.models.ASRModel.from_pretrained")
        transcribe = script.index("result = self._call_transcribe(path)")
        self.assertLess(script.rfind("with _nemo_output_context():", 0, load), load)
        self.assertLess(script.rfind("with _nemo_output_context():", 0, transcribe), transcribe)


class PrivacyModeTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_LOCAL_ONLY", "HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE",
            "HF_DATASETS_OFFLINE", "HF_HUB_DISABLE_TELEMETRY",
            "WANDB_DISABLED", "WANDB_MODE",
        )}
        for n in ("vp_privacy", "vp_config", "vp_cli", "vp_transcribe"):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k in self._old:
            os.environ.pop(k, None)
        for k, v in self._old.items():
            if v is not None:
                os.environ[k] = v
        for n in ("vp_privacy", "vp_config", "vp_cli", "vp_transcribe"):
            sys.modules.pop(n, None)

    def test_local_only_applies_offline_environment_gates(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        self.assertTrue(vp_privacy.apply_local_only_network_lock())
        self.assertEqual(os.environ["HF_HUB_OFFLINE"], "1")
        self.assertEqual(os.environ["TRANSFORMERS_OFFLINE"], "1")
        self.assertEqual(os.environ["HF_DATASETS_OFFLINE"], "1")
        self.assertEqual(os.environ["HF_HUB_DISABLE_TELEMETRY"], "1")
        self.assertEqual(os.environ["WANDB_DISABLED"], "true")
        self.assertEqual(os.environ["WANDB_MODE"], "offline")

    def test_local_only_does_not_override_existing_offline_values(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        os.environ["HF_HUB_OFFLINE"] = "custom"
        import vp_privacy

        vp_privacy.apply_local_only_network_lock()

        self.assertEqual(os.environ["HF_HUB_OFFLINE"], "custom")

    def test_local_only_blocks_cloud_backends(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_privacy.assert_local_backend("openai:gpt-4o-transcribe")

    def test_local_only_allows_current_local_backends(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_privacy

        for backend in ("whisper", "faster-whisper", "parakeet"):
            vp_privacy.assert_local_backend(backend)

    def test_local_only_is_registered_as_config_setting(self):
        import vp_config

        self.assertIn("VOICEPI_LOCAL_ONLY", vp_config.SETTING_BY_ENV)
        self.assertEqual(
            vp_config.SETTING_BY_ENV["VOICEPI_LOCAL_ONLY"].key,
            "local_only",
        )
        self.assertFalse(vp_config.SETTING_BY_ENV["VOICEPI_LOCAL_ONLY"].live)

    def test_debug_dump_reports_local_only_setting(self):
        with open("vp_cli.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('"local only"', script)
        self.assertIn("VOICEPI_LOCAL_ONLY", script)


class PostprocessTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_POST_PROCESSOR", "VOICEPI_POST_MODE", "VOICEPI_POST_MODEL",
            "VOICEPI_POST_BASE_URL", "VOICEPI_POST_TIMEOUT_MS",
            "VOICEPI_POST_MAX_INPUT_CHARS", "VOICEPI_POST_MAX_OUTPUT_CHARS",
            "VOICEPI_POST_API_KEY", "OPENAI_API_KEY", "VOICEPI_LOCAL_ONLY",
        )}
        for n in ("vp_postprocess", "vp_config", "vp_privacy", "vp_external_api"):
            sys.modules.pop(n, None)

    def tearDown(self):
        for k in self._old:
            os.environ.pop(k, None)
        for k, v in self._old.items():
            if v is not None:
                os.environ[k] = v
        for n in ("vp_postprocess", "vp_config", "vp_privacy", "vp_external_api"):
            sys.modules.pop(n, None)

    def test_raw_mode_returns_text_unchanged(self):
        import vp_postprocess

        result = vp_postprocess.postprocess_text("keep this")

        self.assertEqual(result.text, "keep this")
        self.assertFalse(result.changed)
        self.assertEqual(result.provider, "none")
        self.assertEqual(result.mode, "raw")

    def test_postprocess_mode_prompts_cover_roadmap_modes(self):
        import vp_postprocess

        expectations = {
            "clean": "Clean punctuation",
            "prompt": "AI coding agent",
            "terminal": "Preserve commands",
            "slack": "Slack-style message",
            "email": "polished but faithful email",
            "bullets": "concise bullet points",
            "bullet-list": "concise bullet points",
        }
        for mode, phrase in expectations.items():
            with self.subTest(mode=mode):
                prompt = vp_postprocess.build_prompt("hello world", mode)
                self.assertIn(phrase, prompt)
                self.assertIn("Return only the rewritten text", prompt)

    def test_postprocess_accepts_bullet_list_alias(self):
        os.environ["VOICEPI_POST_PROCESSOR"] = "ollama"
        os.environ["VOICEPI_POST_MODE"] = "bullet-list"
        import vp_postprocess

        settings = vp_postprocess.load_postprocess_settings()

        self.assertEqual(settings.mode, "bullets")
        result = vp_postprocess.postprocess_text("fallback", vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="bullet-list",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        ))
        self.assertEqual(result.mode, "bullets")
        self.assertTrue(result.fallback)

    def test_clean_mode_uses_fake_ollama_server(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["payload"] = json.loads(body.decode("utf-8"))
                data = json.dumps({"response": "Hello, world."}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            model="qwen2.5:3b",
            base_url=f"http://127.0.0.1:{server.server_port}",
        )
        result = vp_postprocess.postprocess_text("hello world", settings)

        self.assertEqual(result.text, "Hello, world.")
        self.assertTrue(result.changed)
        self.assertEqual(result.model, "qwen2.5:3b")
        self.assertEqual(calls["path"], "/api/generate")
        self.assertEqual(calls["payload"]["model"], "qwen2.5:3b")
        self.assertIn("Clean punctuation", calls["payload"]["prompt"])

    def test_openai_postprocessor_uses_fake_chat_server(self):
        import threading
        from http.server import BaseHTTPRequestHandler, HTTPServer

        calls = {}

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                calls["path"] = self.path
                calls["auth"] = self.headers.get("Authorization")
                calls["payload"] = json.loads(body.decode("utf-8"))
                data = json.dumps({
                    "choices": [{
                        "message": {"content": "Cleaned text."}
                    }]
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            model="gpt-4o-mini",
            base_url=f"http://127.0.0.1:{server.server_port}/v1",
            api_key="test-key",
        )
        result = vp_postprocess.postprocess_text("cleaned text", settings)

        self.assertEqual(result.text, "Cleaned text.")
        self.assertEqual(result.provider, "openai")
        self.assertEqual(calls["path"], "/v1/chat/completions")
        self.assertEqual(calls["auth"], "Bearer test-key")
        self.assertIn("Clean punctuation", calls["payload"]["messages"][1]["content"])

    def test_ollama_failure_falls_back_to_original_text(self):
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="http://127.0.0.1:1",
            timeout_ms=100,
        )
        result = vp_postprocess.postprocess_text("fallback text", settings)

        self.assertEqual(result.text, "fallback text")
        self.assertTrue(result.fallback)
        self.assertTrue(result.error)

    def test_local_only_blocks_remote_postprocess_url(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="https://example.com",
        )

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_postprocess.validate_postprocess_settings(settings)

    def test_local_only_blocks_openai_postprocessor_even_on_localhost(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="openai",
            mode="clean",
            base_url="http://localhost:11434",
            api_key="test-key",
        )

        with self.assertRaisesRegex(RuntimeError, "VOICEPI_LOCAL_ONLY=1"):
            vp_postprocess.validate_postprocess_settings(settings)

    def test_local_only_allows_localhost_postprocess_url(self):
        os.environ["VOICEPI_LOCAL_ONLY"] = "1"
        import vp_postprocess

        settings = vp_postprocess.PostprocessSettings(
            processor="ollama",
            mode="clean",
            base_url="http://localhost:11434",
        )

        vp_postprocess.validate_postprocess_settings(settings)

    def test_voice_pi_records_postprocess_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("postprocess_text(text", script)
        self.assertIn("dictionary_text=text", script)
        self.assertIn("post_processor=post_result.provider", script)
        self.assertIn("post_fallback=post_result.fallback", script)


class CommandHookTests(unittest.TestCase):
    def test_command_hook_sends_event_json_on_stdin(self):
        import vp_command_hook

        with tempfile.NamedTemporaryFile(delete=False) as f:
            out_path = f.name
        code = (
            "import json,sys,pathlib;"
            "event=json.loads(sys.stdin.read());"
            f"pathlib.Path({out_path!r}).write_text(event['text'], encoding='utf-8')"
        )
        command = json.dumps([sys.executable, "-c", code])
        try:
            with patch.dict(os.environ, {
                "VOICEPI_COMMAND_HOOK": command,
                "VOICEPI_COMMAND_HOOK_TIMEOUT_MS": "2000",
            }, clear=False):
                result = vp_command_hook.run_command_hook({"text": "hello Codex"})
            with open(out_path, encoding="utf-8") as f:
                written = f.read()
        finally:
            os.remove(out_path)

        self.assertTrue(result.enabled)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(written, "hello Codex")

    def test_command_hook_timeout_is_nonfatal(self):
        import vp_command_hook

        command = json.dumps([sys.executable, "-c", "import time; time.sleep(2)"])
        with patch.dict(os.environ, {
            "VOICEPI_COMMAND_HOOK": command,
            "VOICEPI_COMMAND_HOOK_TIMEOUT_MS": "100",
        }, clear=False):
            result = vp_command_hook.run_command_hook({"text": "slow"})

        self.assertTrue(result.enabled)
        self.assertTrue(result.timeout)
        self.assertIn("timed out", result.error)

    def test_command_hook_rejects_invalid_json_array(self):
        import vp_command_hook

        with patch.dict(os.environ, {"VOICEPI_COMMAND_HOOK": '["ok", 5]'}, clear=False):
            result = vp_command_hook.run_command_hook({"text": "bad"})

        self.assertTrue(result.enabled)
        self.assertIn("array of strings", result.error)

    def test_voice_pi_records_command_hook_after_event_creation(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        event_pos = script.index("event = base_event(")
        hook_pos = script.index("hook_result = run_command_hook(event)")
        metrics_pos = script.index("append_jsonl(self.metrics_jsonl, event)")
        self.assertLess(event_pos, hook_pos)
        self.assertLess(hook_pos, metrics_pos)


class FormatCommandTests(unittest.TestCase):
    def test_format_commands_are_off_by_default(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands("write comma literally")

        self.assertFalse(result.enabled)
        self.assertEqual(result.text, "write comma literally")

    def test_english_format_commands_replace_whole_phrases(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "first item comma new line second item period", "en")

        self.assertTrue(result.enabled)
        self.assertTrue(result.changed)
        self.assertEqual(result.text, "first item,\nsecond item.")
        self.assertIn({"command": "new line", "replacement": "\n", "count": "1"}, result.applied)

    def test_danish_format_commands_replace_whole_phrases(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "første punkt komma ny linje andet punkt punktum", "da")

        self.assertEqual(result.text, "første punkt,\nandet punkt.")

    def test_format_commands_do_not_replace_inside_words(self):
        import vp_formatting

        result = vp_formatting.apply_format_commands(
            "Common words and kommandolinje stay literal", "both")

        self.assertFalse(result.changed)
        self.assertEqual(result.text, "Common words and kommandolinje stay literal")

    def test_voice_pi_applies_formatting_before_injection_and_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        post_pos = script.index("post_result = postprocess_text")
        format_pos = script.index("format_result = apply_format_commands")
        inject_pos = script.index("self._inject(final_text)")
        metrics_pos = script.index("format_commands_applied=format_result.applied")
        self.assertLess(post_pos, format_pos)
        self.assertLess(format_pos, inject_pos)
        self.assertLess(inject_pos, metrics_pos)

class DictionarySuggestTests(unittest.TestCase):
    def test_suggests_replacements_from_benchmark_term_misses(self):
        import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        rows = [{
            "corpus_id": "da-tech-004",
            "text": "Murch branchedes og de plåede den nye version bagefter.",
            "reference_text": "Merge branchen og deploy den nye version bagefter.",
            "term_misses": ["merge", "deploy"],
            "reference_terms": ["merge", "deploy"],
        }]

        try:
            vp_dictionary_suggest.DICTIONARY.terms = []
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                rows, min_confidence=0.55)
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        pairs = {(s.source.casefold(), s.target.casefold()) for s in suggestions}
        self.assertIn(("murch", "merge"), pairs)
        self.assertNotIn(("de", "deploy"), pairs)

    def test_suggest_filters_common_word_sources(self):
        import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        rows = [{
            "corpus_id": "sample",
            "text": "og de til le as skal Code Large MCP Claude Set",
            "reference_text": "vLLM deploy type Codex bullets Hetzner Codex large v3",
            "term_misses": [
                "vLLM", "deploy", "type", "Codex", "bullets", "Hetzner",
                "large v3", "RAG", "Claude Code", "STT",
            ],
        }]

        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["MCP", "Claude"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                rows, min_confidence=0.55)
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        sources = {s.source.casefold() for s in suggestions}
        self.assertFalse(sources & {
            "og", "de", "til", "le", "as", "skal", "code", "large",
            "mcp", "claude", "set", "mig", "køre", "typ",
        })

    def test_suggests_dictionary_term_near_misses(self):
        import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["Claude Code"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "Clort kode should work", "corpus_id": "sample"}],
                min_confidence=0.45,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertTrue(any(s.target == "Claude Code" for s in suggestions))

    def test_benchmark_rows_without_misses_do_not_scan_whole_dictionary(self):
        import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["AMD"]
            vp_dictionary_suggest.DICTIONARY.replacements = {}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{
                    "text": "and then continue",
                    "reference_text": "and then continue",
                    "reference_terms": [],
                    "term_misses": [],
                }],
                min_confidence=0.5,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertEqual(suggestions, [])

    def test_suggest_does_not_duplicate_existing_replacements(self):
        import vp_dictionary_suggest

        old_terms = vp_dictionary_suggest.DICTIONARY.terms
        old_replacements = vp_dictionary_suggest.DICTIONARY.replacements
        try:
            vp_dictionary_suggest.DICTIONARY.terms = ["lead dev"]
            vp_dictionary_suggest.DICTIONARY.replacements = {"lead death": "lead dev"}
            suggestions = vp_dictionary_suggest.suggest_replacements_from_rows(
                [{"text": "lead death", "term_misses": ["lead dev"]}],
                min_confidence=0.5,
            )
        finally:
            vp_dictionary_suggest.DICTIONARY.terms = old_terms
            vp_dictionary_suggest.DICTIONARY.replacements = old_replacements

        self.assertFalse(any(s.source == "lead death" and s.target == "lead dev"
                             for s in suggestions))

    def test_parser_accepts_dictionary_suggest(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--dictionary-suggest", "benchmark/results.jsonl",
            "--dictionary-suggest-min-confidence", "0.7",
        ])

        self.assertEqual(args.dictionary_suggest, "benchmark/results.jsonl")
        self.assertEqual(args.dictionary_suggest_min_confidence, 0.7)


class WindowsLauncherRegressionTests(unittest.TestCase):
    def test_installer_no_longer_packages_legacy_launchers(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        for legacy in (
            "setup.ps1",
            "setup.cmd",
            "settings-ui.ps1",
            "settings-ui.vbs",
            "requirements-ui.txt",
            "Legacy Settings UI",
            "whisper-dictate Terminal",
        ):
            self.assertNotIn(legacy, script)
        self.assertIn(r'Source: "..\target\release\whisper-dictate.exe"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"', script)

    def test_rust_windows_ui_uses_gui_subsystem(self):
        script = Path("crates/whisper-dictate-app/src/main.rs").read_text(encoding="utf-8")

        self.assertIn(
            '#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]',
            script,
        )

    def test_rust_background_processes_hide_windows_console(self):
        script = Path("crates/whisper-dictate-app/src/runtime.rs").read_text(encoding="utf-8")

        self.assertIn("const CREATE_NO_WINDOW: u32 = 0x08000000;", script)
        self.assertIn("fn configure_background_process(command: &mut Command)", script)
        self.assertIn("command.creation_flags(CREATE_NO_WINDOW);", script)
        self.assertIn("configure_background_process(&mut process);", script)

    def test_rust_runtime_log_expands_to_available_width(self):
        script = Path("crates/whisper-dictate-app/src/ui.rs").read_text(encoding="utf-8")

        self.assertIn(".desired_width(ui.available_width())", script)
        self.assertIn(".min_size(egui::vec2(ui.available_width(), height))", script)
        self.assertIn(".auto_shrink([false, false])", script)

    def test_rust_runtime_tab_can_clear_log_without_stopping_runtime(self):
        script = Path("crates/whisper-dictate-app/src/ui.rs").read_text(encoding="utf-8")

        self.assertIn('ui.button("Clear").clicked()', script)
        self.assertIn("self.runtime_log.clear();", script)

    def test_rust_ui_has_groq_cloud_stt_preset_and_key_link(self):
        script = Path("crates/whisper-dictate-app/src/ui.rs").read_text(encoding="utf-8")

        self.assertIn('GROQ_STT_BASE_URL: &str = "https://api.groq.com/openai/v1"', script)
        self.assertIn('GROQ_STT_MODEL: &str = "whisper-large-v3-turbo"', script)
        self.assertIn('GROQ_KEYS_URL: &str = "https://console.groq.com/keys"', script)
        self.assertIn('ui.button("Use Groq cloud STT").clicked()', script)
        self.assertIn('ui.button("Groq API keys").clicked()', script)
        self.assertIn('open_url(GROQ_KEYS_URL)', script)
        self.assertIn('GROQ_API_KEY, VOICEPI_STT_API_KEY or OPENAI_API_KEY', script)

    def test_windows_docs_use_rust_terminal_entrypoint(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("runs the Rust UI and starts the Python worker hidden underneath it", readme)
        self.assertIn("whisper-dictate run --key ctrl_r --lang da", readme)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da --device cuda", readme)
        self.assertIn("whisper-dictate.exe\" run --key ctrl_r --lang da --model large-v3 --device cuda", config)
        self.assertIn(r"whisper-dictate.exe run --key ctrl_r --lang da", config)
        self.assertIn("Rust UI is the installer Start-menu", technical)
        self.assertIn("no compatibility script is installed", technical)
        self.assertNotIn("whisper-dictate Terminal", readme)
        self.assertNotIn("whisper-dictate Debug Terminal", readme)
        self.assertNotIn("Current primary path is the installed PySide/PowerShell UI", technical)

    def test_docs_describe_groq_as_explicit_opt_in_without_storing_keys(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")

        for doc in (readme, config):
            self.assertIn("https://api.groq.com/openai/v1", doc)
            self.assertIn("whisper-large-v3-turbo", doc)
            self.assertIn("GROQ_API_KEY", doc)
        self.assertIn("Use Groq cloud STT", config)
        self.assertIn("API keys are not saved in the Settings UI config file", readme)

    def test_installer_uses_whisper_dictate_icon_and_searchable_ui_name(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r"SetupIconFile=..\assets\whisper-dictate.ico", script)
        self.assertIn(r'Source: "..\assets\whisper-dictate.ico"', script)
        self.assertIn(r'IconFilename: "{app}\whisper-dictate.ico"', script)
        self.assertNotIn(r"Legacy Settings UI", script)
        self.assertNotIn(r"\Settings UI", script)

    def test_windows_icon_is_multiresolution_and_has_source_logo(self):
        icon = Path("assets/whisper-dictate.ico").read_bytes()
        svg = Path("assets/whisper-dictate-logo.svg").read_text(encoding="utf-8")

        self.assertGreater(len(icon), 90_000)
        self.assertEqual(int.from_bytes(icon[0:2], "little"), 0)
        self.assertEqual(int.from_bytes(icon[2:4], "little"), 1)
        self.assertEqual(int.from_bytes(icon[4:6], "little"), 7)
        sizes = {
            256 if icon[6 + i * 16] == 0 else icon[6 + i * 16]
            for i in range(7)
        }
        self.assertEqual(sizes, {16, 24, 32, 48, 64, 128, 256})
        self.assertIn("viewBox=\"0 0 256 256\"", svg)
        self.assertIn("linearGradient", svg)
        self.assertIn("fill=\"#FFFFFF\"", svg)

    def test_github_docs_show_logo(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        release_notes = Path("RELEASE_NOTES.md").read_text(encoding="utf-8")

        self.assertIn('src="assets/whisper-dictate-logo.svg"', readme)
        self.assertIn("<h1 align=\"center\">whisper-dictate</h1>", readme)
        self.assertIn('src="assets/whisper-dictate-logo.svg"', release_notes)

    def test_installer_creates_desktop_ui_shortcut(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Name: "{userdesktop}\whisper-dictate"', script)
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"', script)
        self.assertIn(r'Parameters: "ui"', script)

    def test_installer_packages_rust_ui_as_primary_desktop_entry(self):
        with open("installer/whisper-dictate.iss", encoding="utf-8") as f:
            script = f.read()

        self.assertIn(r'Source: "..\target\release\whisper-dictate.exe"', script)
        self.assertIn(
            r'Name: "{userprograms}\whisper-dictate\whisper-dictate";    Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"',
            script,
        )
        self.assertIn(r'Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; Description: "Launch whisper-dictate now"', script)

    def test_windows_installer_workflows_build_rust_ui_before_inno(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")
            rust_build = workflow.index("cargo build --release -p whisper-dictate-app")
            installer_build = workflow.index("Build installers")
            self.assertLess(rust_build, installer_build)
            self.assertIn("Cargo.toml Cargo.lock crates/", workflow)

        script = Path("scripts/build-windows-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("cargo build --release -p whisper-dictate-app", script)
        self.assertIn("cargo build failed", script)

    def test_windows_zip_packages_are_built_on_windows_with_rust_exe(self):
        for path in (".github/workflows/release.yml", ".github/workflows/windows-installer.yml"):
            workflow = Path(path).read_text(encoding="utf-8")

            self.assertIn("Build Windows ZIP packages", workflow)
            self.assertIn("whisper-dictate-windows-$variant-$version.zip", workflow)
            self.assertIn("Copy-Item target\\release\\whisper-dictate.exe", workflow)
            self.assertIn("Copy-Item assets\\whisper-dictate.ico", workflow)
            self.assertIn('Copy-Item $variants[$variant] (Join-Path $bundle "requirements.txt")', workflow)
            self.assertIn("Output/*.exe Output/*.zip sha256sums.txt", workflow)

        script = Path("scripts/build-windows-installer.ps1").read_text(encoding="utf-8")
        self.assertIn("Building $v portable ZIP version $Version", script)
        self.assertIn("whisper-dictate-windows-$v-$Version.zip", script)
        self.assertIn("target\\release\\whisper-dictate.exe", script)
        self.assertIn("assets\\whisper-dictate.ico", script)
        self.assertIn("Compress-Archive", script)

    def test_docs_describe_windows_zip_and_installer_outputs(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        release_notes = Path("RELEASE_NOTES.md").read_text(encoding="utf-8")
        agents = Path("AGENTS.md").read_text(encoding="utf-8")
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("portable Windows ZIP bundles", readme)
        self.assertIn("installer and portable ZIP are written to `Output\\`", readme)
        self.assertIn("whisper-dictate-windows-nvidia-<version>.zip", release_notes)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", agents)
        self.assertIn("Output\\*.exe` and `Output\\*.zip", technical)

    def test_voice_pi_reconfigures_windows_streams_to_utf8(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn('reconfigure(encoding="utf-8", errors="replace")', script)

    def test_voice_pi_has_parakeet_min_duration_and_backend_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.parakeet_min_seconds", script)
        self.assertIn("too short for Parakeet", script)
        self.assertIn("stt_backend=self.stt_backend", script)

    def test_voice_pi_has_live_release_tail_padding(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("self.release_tail_ms", script)
        self.assertIn('after.get("release_tail_ms", "200")', script)
        self.assertIn("time.sleep(tail_s)", script)

    def test_cli_debug_prints_parakeet_min_seconds(self):
        with open("vp_cli.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("parakeet_min_s", script)
        self.assertIn("VOICEPI_PARAKEET_MIN_SECONDS", script)
        self.assertIn("release_tail_ms", script)
        self.assertIn("VOICEPI_RELEASE_TAIL_MS", script)


class TranscribeFileTests(unittest.TestCase):
    def _write_test_wav(self, path, *, rate=16000, seconds=0.8):
        import numpy as np

        t = np.linspace(0, seconds, int(rate * seconds), endpoint=False)
        audio = (0.25 * np.sin(2 * np.pi * 440 * t)).astype(np.float32)
        pcm = (audio * 32767).astype(np.int16)
        with wave.open(path, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(rate)
            wav.writeframes(pcm.tobytes())

    def test_parser_accepts_transcribe_file(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args(
            ["--transcribe-file", "sample.wav"])
        self.assertEqual(args.transcribe_file, "sample.wav")

    def test_load_audio_file_decodes_wav_as_16khz_int16_mono(self):
        import vp_file_transcribe

        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path, rate=8000)
            pcm = vp_file_transcribe.load_audio_file(path)
        finally:
            os.remove(path)

        self.assertEqual(pcm.dtype.name, "int16")
        self.assertEqual(pcm.ndim, 2)
        self.assertEqual(pcm.shape[1], 1)
        self.assertGreaterEqual(len(pcm), 12000)

    def test_transcribe_file_event_uses_dictionary_replacements(self):
        import vp_file_transcribe
        import vp_transcribe

        class Segment:
            text = " lead death"
            start = 0.0
            end = 0.8

        class Info:
            language = "en"
            language_probability = 0.9

        class Model:
            def transcribe(self, *_args, **_kwargs):
                return [Segment()], Info()

        class Dict:
            def build_prompt(self, prompt):
                return prompt

            def apply_replacements(self, text):
                return text.replace("lead death", "lead dev"), [
                    {"from": "lead death", "to": "lead dev", "count": 1}
                ]

            def prompt_terms(self):
                return ["lead dev"]

        old_dict = vp_transcribe.DICTIONARY
        old_gate = vp_transcribe._looks_like_speech
        vp_transcribe.DICTIONARY = Dict()
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            event = vp_file_transcribe.transcribe_file_event(
                Model(), path, "en",
                model_name="fake", stt_backend="whisper",
                device="cpu", compute_type="int8",
            )
        finally:
            vp_transcribe.DICTIONARY = old_dict
            vp_transcribe._looks_like_speech = old_gate
            os.remove(path)

        self.assertEqual(event["event"], "file_transcription")
        self.assertEqual(event["text"], "lead dev")
        self.assertEqual(event["raw_text"], "lead death")
        self.assertEqual(event["source_file"], path)
        self.assertEqual(event["dictionary_terms"], ["lead dev"])
        self.assertEqual(event["dictionary_replacements"][0]["from"], "lead death")

    def test_transcribe_file_json_output_is_single_json_object(self):
        import vp_file_transcribe

        event = {"event": "file_transcription", "text": "hello"}
        with _capture_stdout() as buf:
            vp_file_transcribe.print_transcribe_file_result(event, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), event)


class BenchmarkTests(unittest.TestCase):
    def test_corpus_manifest_loads_and_scores_terms(self):
        import vp_corpus

        item = vp_corpus.load_corpus("benchmark/corpus.json")[0]

        self.assertEqual(item.id, "da-short-001")
        self.assertTrue(str(item.audio).endswith("benchmark\\audio\\da-short-001.wav") or
                        str(item.audio).endswith("benchmark/audio/da-short-001.wav"))
        self.assertEqual(vp_corpus.wer("Claude Code virker", "Claude virker"), 1 / 3)
        report = vp_corpus.term_report(["Claude Code", "Codex"], "Claude Code works")
        self.assertEqual(report["hits"], ["Claude Code"])
        self.assertEqual(report["misses"], ["Codex"])

    def test_corpus_annotates_benchmark_event(self):
        import vp_corpus

        item = vp_corpus.CorpusItem(
            id="x",
            text="Claude Code and Codex",
            audio=Path("x.wav"),
            language="en",
            category="tech",
            terms=("Claude Code", "Codex"),
        )
        event = vp_corpus.annotate_event({"text": "Claude Code and codec"}, item)

        self.assertEqual(event["corpus_id"], "x")
        self.assertEqual(event["corpus_language"], "en")
        self.assertGreater(event["wer"], 0)
        self.assertEqual(event["term_hits"], ["Claude Code"])
        self.assertEqual(event["term_misses"], ["Codex"])

    def test_parse_backend_specs_supports_models(self):
        import vp_benchmark

        specs = vp_benchmark.parse_backend_specs(
            "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3,openai:gpt-4o-mini-transcribe")

        self.assertEqual(specs[0].backend, "whisper")
        self.assertEqual(specs[0].model, "large-v3")
        self.assertEqual(specs[1].backend, "parakeet")
        self.assertEqual(specs[1].model, "nvidia/parakeet-tdt-0.6b-v3")
        self.assertEqual(specs[2].backend, "openai")
        self.assertEqual(specs[2].model, "gpt-4o-mini-transcribe")

    def test_parse_backend_specs_rejects_unknown_backend(self):
        import vp_benchmark

        with self.assertRaisesRegex(ValueError, "unsupported benchmark backend"):
            vp_benchmark.parse_backend_specs("cloud:gpt-4o-transcribe")

    def test_benchmark_run_one_invokes_transcribe_file_json(self):
        import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=0,
            stdout='{"event":"file_transcription","text":"hello"}\n',
            stderr="",
        )
        with patch("vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="whisper:large-v3", backend="whisper", model="large-v3"),
                python_exe="python",
                app_path="voice_pi.py",
                base_env={},
            )

        cmd = run.call_args.args[0]
        env = run.call_args.kwargs["env"]
        self.assertEqual(cmd, [
            "python", "voice_pi.py", "--transcribe-file", "sample.wav", "--json"
        ])
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "whisper")
        self.assertEqual(env["VOICEPI_MODEL"], "large-v3")
        self.assertTrue(event["benchmark_success"])
        self.assertEqual(event["benchmark_backend_spec"], "whisper:large-v3")

    def test_benchmark_parakeet_model_uses_parakeet_env(self):
        import vp_benchmark

        completed = types.SimpleNamespace(
            returncode=1,
            stdout="",
            stderr="missing nemo",
        )
        with patch("vp_benchmark.subprocess.run", return_value=completed) as run:
            event = vp_benchmark.run_one(
                "sample.wav",
                vp_benchmark.BackendSpec(
                    raw="parakeet:nvidia/model", backend="parakeet",
                    model="nvidia/model"),
                python_exe="python",
                app_path="voice_pi.py",
                base_env={},
            )

        env = run.call_args.kwargs["env"]
        self.assertEqual(env["VOICEPI_STT_BACKEND"], "parakeet")
        self.assertEqual(env["VOICEPI_PARAKEET_MODEL"], "nvidia/model")
        self.assertFalse(event["benchmark_success"])
        self.assertIn("missing nemo", event["benchmark_error"])

    def test_benchmark_jsonl_writes_one_line_per_file_backend(self):
        import vp_benchmark

        events = []

        def fake_run_one(audio_file, spec):
            event = {
                "event": "benchmark_result",
                "source_file": str(audio_file),
                "benchmark_backend_spec": spec.raw,
                "text": "ok",
            }
            events.append(event)
            return event

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            with patch("vp_benchmark.run_one", side_effect=fake_run_one):
                results = vp_benchmark.run_benchmark(
                    ["a.wav", "b.wav"], "whisper,parakeet", output_jsonl=path)
            with open(path, encoding="utf-8") as f:
                lines = [json.loads(line) for line in f]
        finally:
            os.remove(path)

        self.assertEqual(len(results), 4)
        self.assertEqual(len(lines), 4)
        self.assertEqual(lines[0]["benchmark_backend_spec"], "whisper")

    def test_benchmark_corpus_writes_skipped_rows_for_missing_audio(self):
        import vp_benchmark

        manifest = {
            "items": [{
                "id": "sample",
                "language": "en",
                "category": "unit",
                "text": "Hello Codex",
                "audio": "missing.wav",
                "terms": ["Codex"],
            }]
        }
        with tempfile.TemporaryDirectory() as d:
            manifest_path = os.path.join(d, "corpus.json")
            output_path = os.path.join(d, "out.jsonl")
            with open(manifest_path, "w", encoding="utf-8") as f:
                json.dump(manifest, f)

            results = vp_benchmark.run_benchmark(
                None,
                "whisper",
                output_jsonl=output_path,
                corpus_manifest=manifest_path,
            )
            with open(output_path, encoding="utf-8") as f:
                rows = [json.loads(line) for line in f]

        self.assertEqual(len(results), 1)
        self.assertEqual(rows[0]["corpus_id"], "sample")
        self.assertTrue(rows[0]["benchmark_skipped"])
        self.assertEqual(rows[0]["benchmark_backend_spec"], "whisper")
        self.assertEqual(rows[0]["benchmark_backend"], "whisper")
        self.assertIsNone(rows[0]["benchmark_model"])
        self.assertEqual(rows[0]["term_misses"], ["Codex"])

    def test_benchmark_corpus_reuses_loaded_model_per_backend(self):
        import vp_benchmark

        manifest = {
            "audio_dir": "audio",
            "items": [
                {"id": "one", "language": "da", "text": "Hej Codex", "terms": ["Codex"]},
                {"id": "two", "language": "en", "text": "Hello Claude Code", "terms": ["Claude Code"]},
            ]
        }
        calls = []

        def fake_transcribe(model, path, lang, **kwargs):
            calls.append((model, Path(path).name, lang, kwargs["stt_backend"]))
            text = "Hej Codex" if lang == "da" else "Hello Claude Code"
            return {"event": "file_transcription", "text": text, "source_file": str(path)}

        with tempfile.TemporaryDirectory() as d:
            manifest_path = Path(d) / "corpus.json"
            audio_dir = Path(d) / "audio"
            audio_dir.mkdir()
            for name in ("one.wav", "two.wav"):
                (audio_dir / name).write_bytes(b"not used by patched transcriber")
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            try:
                with patch("vp_benchmark._load_model_for_spec", return_value=("model", "m", "cpu", "int8")) as load:
                    with patch("vp_file_transcribe.transcribe_file_event", side_effect=fake_transcribe):
                        results = vp_benchmark.run_benchmark(
                            None,
                            "whisper:tiny",
                            corpus_manifest=manifest_path,
                        )
            finally:
                sys.modules.pop("vp_file_transcribe", None)
                sys.modules.pop("vp_transcribe", None)

        self.assertEqual(load.call_count, 1)
        self.assertEqual(len(calls), 2)
        self.assertEqual([c[2] for c in calls], ["da", "en"])
        self.assertTrue(all(r["benchmark_success"] for r in results))
        self.assertEqual(results[1]["term_hits"], ["Claude Code"])

    def test_record_corpus_imports_sounddevice_lazily_with_help(self):
        with open("scripts/record-corpus.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def load_sounddevice", script)
        self.assertIn("Missing recorder dependency: sounddevice", script)
        self.assertIn("py -3.12 -m pip install", script)
        self.assertIn("sounddevice>=0.4,<0.6", script)
        self.assertIn("sd = load_sounddevice()", script)
        self.assertNotIn("\nimport sounddevice as sd\n", script)

    def test_parser_accepts_benchmark_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args([
            "--benchmark-files", "a.wav", "b.wav",
            "--benchmark-corpus", "benchmark/corpus.json",
            "--benchmark-backends", "whisper,parakeet",
            "--benchmark-jsonl", "out.jsonl",
        ])

        self.assertEqual(args.benchmark_files, ["a.wav", "b.wav"])
        self.assertEqual(args.benchmark_corpus, "benchmark/corpus.json")
        self.assertEqual(args.benchmark_backends, "whisper,parakeet")
        self.assertEqual(args.benchmark_jsonl, "out.jsonl")


class CalibrationTests(unittest.TestCase):
    def test_calibration_analysis_passes_clean_audio(self):
        np = AudioDspTests.np
        sys.modules["numpy"] = np
        sys.modules.pop("vp_calibration", None)
        import vp_calibration

        quiet = np.full(4000, 0.001, dtype=np.float32)
        speech = np.full(12000, 0.15, dtype=np.float32)
        pcm = (np.concatenate([quiet, speech]) * 32767).astype(np.int16)

        result = vp_calibration.analyze_calibration_audio(pcm)

        self.assertEqual(result["event"], "mic_calibration")
        self.assertEqual(result["status"], "pass")
        self.assertGreater(result["snr_db"], 15)
        self.assertIn("VOICEPI_MIN_INPUT_DBFS", result["recommended"])

    def test_calibration_analysis_warns_on_low_snr(self):
        np = AudioDspTests.np
        sys.modules["numpy"] = np
        sys.modules.pop("vp_calibration", None)
        import vp_calibration

        pcm = (np.full(16000, 0.01, dtype=np.float32) * 32767).astype(np.int16)

        result = vp_calibration.analyze_calibration_audio(pcm)

        self.assertIn(result["status"], ("warn", "fail"))
        self.assertTrue(result["warnings"])

    def test_calibration_json_output_is_single_json_object(self):
        import vp_calibration

        result = {
            "event": "mic_calibration",
            "status": "pass",
            "warnings": [],
            "raw_dbfs": -20.0,
            "noise_dbfs": -60.0,
            "snr_db": 40.0,
            "peak": 0.5,
            "recommended": {},
        }
        with _capture_stdout() as buf:
            vp_calibration.print_calibration_result(result, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), result)

    def test_parser_accepts_calibration_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic"])
        self.assertEqual(args.calibrate_mic, 5.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-mic", "3"])
        self.assertEqual(args.calibrate_mic, 3.0)
        args = vp_cli.build_arg_parser().parse_args(["--calibrate-file", "sample.wav"])
        self.assertEqual(args.calibrate_file, "sample.wav")

    def test_voice_pi_handles_calibration_before_model_load(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        calibration = script.index("if a.calibrate_mic is not None or a.calibrate_file")
        model_load = script.index("_model = load_stt_model")
        self.assertLess(calibration, model_load)


class HistoryTests(unittest.TestCase):
    def test_append_and_read_history_keeps_core_fields(self):
        import vp_history

        event = {
            "ts": 1,
            "event": "utterance",
            "text": "hello",
            "raw_text": "hallo",
            "stt_backend": "whisper",
            "target_title": "Editor",
            "large_unused_blob": "drop",
        }
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            os.remove(path)
            vp_history.append_history(event, Path(path))
            rows = vp_history.read_history(10, Path(path))
        finally:
            try:
                os.remove(path)
            except OSError:
                pass

        self.assertEqual(rows[0]["text"], "hello")
        self.assertEqual(rows[0]["raw_text"], "hallo")
        self.assertEqual(rows[0]["target_title"], "Editor")
        self.assertNotIn("large_unused_blob", rows[0])

    def test_history_last_returns_latest_item(self):
        import vp_history

        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "first"}) + "\n")
            f.write(json.dumps({"text": "second"}) + "\n")
        try:
            item = vp_history.last_history(Path(path))
        finally:
            os.remove(path)

        self.assertEqual(item["text"], "second")

    def test_history_can_be_disabled(self):
        old = os.environ.get("VOICEPI_HISTORY_ENABLED")
        os.environ["VOICEPI_HISTORY_ENABLED"] = "0"
        sys.modules.pop("vp_history", None)
        import vp_history

        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            os.remove(path)
            written = vp_history.append_history({"text": "hidden"}, Path(path))
            self.assertIsNone(written)
            self.assertFalse(os.path.exists(path))
        finally:
            if old is None:
                os.environ.pop("VOICEPI_HISTORY_ENABLED", None)
            else:
                os.environ["VOICEPI_HISTORY_ENABLED"] = old
            sys.modules.pop("vp_history", None)

    def test_history_copy_last_uses_clipboard(self):
        import vp_history

        copied = {}

        fake_pyperclip = types.ModuleType("pyperclip")
        fake_pyperclip.copy = lambda text: copied.setdefault("text", text)
        sys.modules["pyperclip"] = fake_pyperclip
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            path = f.name
            f.write(json.dumps({"text": "copy me"}) + "\n")
        try:
            text = vp_history.copy_last_to_clipboard(Path(path))
        finally:
            os.remove(path)
            sys.modules.pop("pyperclip", None)

        self.assertEqual(text, "copy me")
        self.assertEqual(copied["text"], "copy me")

    def test_parser_accepts_history_options(self):
        sys.modules.pop("vp_cli", None)
        import vp_cli

        args = vp_cli.build_arg_parser().parse_args(["--history-list"])
        self.assertEqual(args.history_list, 10)
        args = vp_cli.build_arg_parser().parse_args(["--history-list", "3"])
        self.assertEqual(args.history_list, 3)
        args = vp_cli.build_arg_parser().parse_args(["--history-last"])
        self.assertTrue(args.history_last)
        args = vp_cli.build_arg_parser().parse_args(["--history-copy-last"])
        self.assertTrue(args.history_copy_last)
        args = vp_cli.build_arg_parser().parse_args(["--history-reinject-last"])
        self.assertTrue(args.history_reinject_last)

    def test_voice_pi_appends_history_after_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("from vp_history import append_history", script)
        self.assertIn("append_history(event)", script)
        self.assertLess(script.index("append_jsonl(self.metrics_jsonl, event)"),
                        script.index("append_history(event)"))


class ProfileTests(unittest.TestCase):
    def test_profile_match_by_title_and_process_applies_settings(self):
        import vp_profiles

        profiles = [{
            "name": "Claude terminal",
            "match": {"title": "Claude Code", "process": "WindowsTerminal"},
            "settings": {"inject_mode": "paste", "lang": "en"},
        }]

        config, name = vp_profiles.apply_profile_settings(
            {"inject_mode": "auto", "lang": "da"},
            profiles,
            title="Claude Code - repo",
            process="WindowsTerminal.exe",
        )

        self.assertEqual(name, "Claude terminal")
        self.assertEqual(config["inject_mode"], "paste")
        self.assertEqual(config["lang"], "en")

    def test_profile_match_returns_default_when_no_match(self):
        import vp_profiles

        config, name = vp_profiles.apply_profile_settings(
            {"inject_mode": "auto"},
            [{"name": "Slack", "match": {"title": "Slack"}, "settings": {"lang": "en"}}],
            title="Codex",
            process="WindowsTerminal.exe",
        )

        self.assertIsNone(name)
        self.assertEqual(config, {"inject_mode": "auto"})

    def test_profile_match_supports_lists(self):
        import vp_profiles

        name, settings = vp_profiles.match_profile(
            [{
                "name": "AI terminals",
                "match": {"title": ["Claude Code", "Codex"]},
                "settings": {"inject_mode": "paste"},
            }],
            title="Codex",
            process=None,
        )

        self.assertEqual(name, "AI terminals")
        self.assertEqual(settings["inject_mode"], "paste")

    def test_voice_pi_records_active_profile_in_metrics(self):
        with open("voice_pi.py", encoding="utf-8") as f:
            script = f.read()

        self.assertIn("def _profiled_config", script)
        self.assertIn("apply_profile_settings", script)
        self.assertIn("[profile] active:", script)
        self.assertIn('profile=getattr(self, "_active_profile_name", None)', script)

    def test_history_keeps_profile_field(self):
        import vp_history

        event = {"text": "hello", "profile": "Claude terminal"}
        stored = vp_history._history_event(event)

        self.assertEqual(stored["profile"], "Claude terminal")


class DictionaryTests(unittest.TestCase):
    def setUp(self):
        self._old = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_DICTIONARY", "VOICEPI_DICTIONARY_ENABLED",
            "VOICEPI_DICTIONARY_MAX_TERMS", "VOICEPI_DICTIONARY_PROMPT_CHARS",
        )}
        sys.modules.pop("vp_dictionary", None)

    def tearDown(self):
        for k in list(self._old):
            os.environ.pop(k, None)
            if self._old[k] is not None:
                os.environ[k] = self._old[k]
        sys.modules.pop("vp_dictionary", None)

    def test_json_dictionary_builds_prompt_and_replacements(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Slack","Claude Code","Codex"],'
                    '"replacements":{"Cloud Code":"Claude Code","code X":"Codex"}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            d = vp_dictionary.DICTIONARY
            self.assertEqual(d.prompt_terms(), ["Slack", "Claude Code", "Codex"])
            self.assertIn("Vocabulary: Slack, Claude Code, Codex",
                          d.build_prompt("Base prompt"))
            text, changes = d.apply_replacements("Open Cloud Code and code X.")
            self.assertEqual(text, "Open Claude Code and Codex.")
            self.assertEqual(len(changes), 2)
        finally:
            os.remove(path)

    def test_text_dictionary_supports_simple_sections(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False, encoding="utf-8") as f:
            f.write("terms:\n- OpenClaw\n- GitHub Actions\n\n"
                    "replacements:\nopen claw => OpenClaw\n")
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            d = vp_dictionary.DICTIONARY
            self.assertIn("OpenClaw", d.terms)
            text, _ = d.apply_replacements("start open claw")
            self.assertEqual(text, "start OpenClaw")
        finally:
            os.remove(path)

    def test_invalid_prompt_limits_fall_back_to_defaults(self):
        import vp_dictionary

        os.environ["VOICEPI_DICTIONARY_MAX_TERMS"] = "bogus"
        os.environ["VOICEPI_DICTIONARY_PROMPT_CHARS"] = "bogus"
        d = vp_dictionary.Dictionary(["Slack", "Claude Code"], {})
        with _capture_stdout() as buf:
            self.assertEqual(d.prompt_terms(), ["Slack", "Claude Code"])
        self.assertIn("ignoring invalid VOICEPI_DICTIONARY_MAX_TERMS", buf.getvalue())

    def test_dictionary_add_term_creates_json_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dictionary.json")
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, added = vp_dictionary.add_dictionary_term("Claude Code")
            _, added_again = vp_dictionary.add_dictionary_term("claude code")
            with open(path, encoding="utf-8") as f:
                data = json.load(f)

        self.assertEqual(str(written), path)
        self.assertTrue(added)
        self.assertFalse(added_again)
        self.assertEqual(data["terms"], ["Claude Code"])
        self.assertEqual(data["replacements"], {})

    def test_dictionary_add_replacement_preserves_terms(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Codex"],"replacements":{}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, src, dst, changed = vp_dictionary.add_dictionary_replacement(
                "code X=Codex")
            with open(path, encoding="utf-8") as f:
                data = json.load(f)
        finally:
            os.remove(path)

        self.assertEqual(str(written), path)
        self.assertEqual((src, dst, changed), ("code X", "Codex", True))
        self.assertEqual(data["terms"], ["Codex"])
        self.assertEqual(data["replacements"], {"code X": "Codex"})

    def test_dictionary_add_replacements_preserves_terms_and_counts_changes(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            f.write('{"terms":["Codex"],"replacements":{"old":"Old"}}')
            path = f.name
        try:
            os.environ["VOICEPI_DICTIONARY"] = path
            import vp_dictionary

            written, changed = vp_dictionary.add_dictionary_replacements({
                "code X": "Codex",
                "old": "Old",
                "": "ignored",
            })
            with open(path, encoding="utf-8") as f:
                data = json.load(f)
        finally:
            os.remove(path)

        self.assertEqual(str(written), path)
        self.assertEqual(changed, 1)
        self.assertEqual(data["terms"], ["Codex"])
        self.assertEqual(data["replacements"], {"code X": "Codex", "old": "Old"})


class RustUiInstallerTests(unittest.TestCase):
    def test_linux_rust_ui_installer_builds_release_binary_and_desktop_entry(self):
        path = Path("scripts/install-linux-rust-ui.sh")
        script = path.read_text(encoding="utf-8")

        self.assertTrue(os.access(path, os.X_OK))
        self.assertIn("cargo build --release -p whisper-dictate-app", script)
        self.assertIn('REAL_BIN="${LIB_DIR}/whisper-dictate-app"', script)
        self.assertIn('install -m 0755 "${HERE}/target/release/whisper-dictate" "${REAL_BIN}"', script)
        self.assertIn('export VOICEPI_APP_ROOT="${HERE}"', script)
        self.assertIn('exec "${REAL_BIN}" "\\$@"', script)
        self.assertIn("whisper-dictate.desktop", script)
        self.assertIn("Exec=${BIN} ui", script)
        self.assertNotIn("setup.ps1", script)

    def test_linux_ui_docs_point_to_rust_ui_not_pyside_powershell(self):
        readme = Path("README.md").read_text(encoding="utf-8")
        config = Path("CONFIGURATION.md").read_text(encoding="utf-8")

        self.assertIn("scripts/install-linux-rust-ui.sh", readme)
        self.assertIn("whisper-dictate ui", readme)
        self.assertIn("scripts/install-linux-rust-ui.sh", config)
        self.assertNotIn("On Linux/macOS, install\n`requirements-ui.txt`", readme)
        self.assertNotIn("setup.ps1 --settings-ui", readme)

    def test_technical_docs_include_rust_platform_capability_matrix(self):
        technical = Path("TECHNICAL.md").read_text(encoding="utf-8")

        self.assertIn("Rust desktop platform capability matrix", technical)
        self.assertIn("| Capability | Windows 10/11 | Linux Wayland | Linux X11 |", technical)
        self.assertIn("whisper-dictate run -- ...", technical)
        self.assertIn("scripts/install-linux-rust-ui.sh", technical)
        self.assertIn("scripts/build-windows-installer.ps1", technical)


class RustReleaseWorkflowTests(unittest.TestCase):
    def test_release_uploads_linux_rust_ui_binary(self):
        workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")

        self.assertIn("cargo build --release -p whisper-dictate-app", workflow)
        self.assertIn("whisper-dictate-linux-rust-ui-${VERSION}", workflow)
        self.assertIn('install -m 0755 target/release/whisper-dictate "$d/whisper-dictate"', workflow)
        self.assertIn('INCLUDE_RUST_UI=1 mkbundle "whisper-dictate-linux-cpu-${VERSION}.zip"', workflow)
        self.assertIn("scripts/install-linux-rust-ui.sh", workflow)
        self.assertIn("bash -n scripts/install-linux-rust-ui.sh", workflow)

    def test_workflows_use_node24_checkout_action(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("actions/checkout@v4", workflow, path.as_posix())
            self.assertIn("actions/checkout@v5", workflow, path.as_posix())

    def test_workflows_use_node24_python_action(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("actions/setup-python@v5", workflow, path.as_posix())
        workflow_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in Path(".github/workflows").glob("*.yml")
        )
        self.assertIn("actions/setup-python@v6", workflow_text)

    def test_windows_workflows_pin_current_windows_runner(self):
        for path in Path(".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("windows-latest", workflow, path.as_posix())
            self.assertNotIn("windows-2025-vs2026", workflow, path.as_posix())
        workflow_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in Path(".github/workflows").glob("*.yml")
        )
        self.assertIn("windows-2025", workflow_text)


if __name__ == "__main__":
    unittest.main()
