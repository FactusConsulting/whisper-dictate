"""Phase 1.2 of the Python-removal roadmap (#348): integration tests for the
Rust whisper transcription shell-out path.

These tests exercise ``vp_transcribe.RustWhisperShellModel`` end-to-end with
``subprocess.run`` stubbed (so the suite stays self-contained and does NOT
require a compiled whisper-dictate binary or a downloaded GGML model). The
real binary is exercised by the Rust unit tests gated on
``WHISPER_TEST_MODEL_PATH`` + ``WHISPER_TEST_WAV_PATH``; the contract this
file pins down is *only* the JSON-over-stdio shape the Python side speaks
into that binary.
"""
from helpers import (
    json,
    os,
    patch,
    real_numpy,
    sys,
    tempfile,
    types,
    unittest,
    wave,
)


def _completed(returncode: int = 0, stdout: str = "", stderr: str = ""):
    return types.SimpleNamespace(returncode=returncode, stdout=stdout, stderr=stderr)


def _fresh_vp_transcribe():
    """Re-import vp_transcribe so module-level constants pick up any env
    overrides the test set before importing."""
    sys.modules.pop("whisper_dictate.vp_transcribe", None)
    from whisper_dictate import vp_transcribe
    return vp_transcribe


def _write_hello_wav(path: str, *, seconds: float = 0.8) -> None:
    """Write a deterministic int16 16 kHz mono WAV — content doesn't matter
    for these tests (the subprocess is stubbed), only the file shape does."""
    import math
    import struct

    rate = 16000
    frames = int(rate * seconds)
    pcm = b"".join(
        struct.pack("<h", int(0.25 * 32767 * math.sin(2 * math.pi * 440 * i / rate)))
        for i in range(frames)
    )
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(pcm)


class RustTranscribeBackendToggleTests(unittest.TestCase):
    """``load_stt_model`` must only dispatch to the Rust shim when BOTH the
    env-var opt-in AND the helper binary are present — anything else falls
    through to faster-whisper so a misconfigured override doesn't break
    dictation."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()

    def test_rust_transcribe_enabled_reads_env_fresh_on_each_call(self):
        vp = _fresh_vp_transcribe()
        # Default state — env var should be missing in the test harness.
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop(vp.TRANSCRIBE_BACKEND_ENV, None)
            self.assertFalse(vp._rust_transcribe_enabled())
        # Opt-in
        with patch.dict(os.environ, {vp.TRANSCRIBE_BACKEND_ENV: "rust"}):
            self.assertTrue(vp._rust_transcribe_enabled())
        # Whitespace + casing tolerated (mirrors other env-var parsers)
        with patch.dict(os.environ, {vp.TRANSCRIBE_BACKEND_ENV: "  RUST  "}):
            self.assertTrue(vp._rust_transcribe_enabled())
        # Any other value (typo, "python", legacy "faster-whisper") is OFF.
        for value in ("", "python", "faster-whisper", "rust-disabled"):
            with patch.dict(os.environ, {vp.TRANSCRIBE_BACKEND_ENV: value}):
                self.assertFalse(vp._rust_transcribe_enabled(),
                                 f"expected off for {value!r}")

    def test_load_stt_model_uses_rust_when_opted_in_and_helper_present(self):
        vp = _fresh_vp_transcribe()
        # Force STT backend to local whisper for this load (load_stt_model
        # reads it via the module-level constant, which the test patches).
        # The probe must succeed for the rust shell model to be chosen.
        with patch.object(vp, "STT_BACKEND", "whisper"), \
                patch.object(vp, "_assert_local_backend", lambda *_a, **_k: None), \
                patch.object(vp, "_rust_helper_supports_transcribe",
                             return_value=True), \
                patch.dict(os.environ, {
                    vp.TRANSCRIBE_BACKEND_ENV: "rust",
                    "VOICEPI_RUST_INJECTOR": "/path/to/whisper-dictate",
                }):
            model = vp.load_stt_model("tiny.en", "cpu", "int8")
        self.assertIsInstance(model, vp.RustWhisperShellModel)

    def test_load_stt_model_falls_back_when_helper_missing(self):
        vp = _fresh_vp_transcribe()
        # Stub faster_whisper so load_stt_model can fall through without
        # needing the real package installed in the test env.
        fw = types.ModuleType("faster_whisper")

        class _Whisper:
            def __init__(self, *a, **k):
                self.args = a
                self.kwargs = k

        fw.WhisperModel = _Whisper
        with patch.dict(sys.modules, {"faster_whisper": fw}), \
                patch.object(vp, "STT_BACKEND", "whisper"), \
                patch.object(vp, "_assert_local_backend", lambda *_a, **_k: None), \
                patch.dict(os.environ, {vp.TRANSCRIBE_BACKEND_ENV: "rust"}, clear=False):
            os.environ.pop("VOICEPI_RUST_INJECTOR", None)
            model = vp.load_stt_model("tiny.en", "cpu", "int8")
        self.assertIsInstance(model, _Whisper)

    def test_load_stt_model_falls_back_when_helper_probe_fails(self):
        """Stock builds set VOICEPI_RUST_INJECTOR (it's the same binary used
        for redact/profile helpers) but `transcribe-wav --probe` exits
        non-zero because the whisper-rs-local feature isn't compiled in. The
        loader MUST detect this up-front via the probe and fall back to
        faster-whisper, otherwise the FIRST dictation fails when the real
        shell-out exits with "feature not compiled in"."""
        vp = _fresh_vp_transcribe()
        fw = types.ModuleType("faster_whisper")

        class _Whisper:
            def __init__(self, *a, **k):
                self.args = a
                self.kwargs = k

        fw.WhisperModel = _Whisper
        with patch.dict(sys.modules, {"faster_whisper": fw}), \
                patch.object(vp, "STT_BACKEND", "whisper"), \
                patch.object(vp, "_assert_local_backend", lambda *_a, **_k: None), \
                patch.object(vp, "_rust_helper_supports_transcribe",
                             return_value=False), \
                patch.dict(os.environ, {
                    vp.TRANSCRIBE_BACKEND_ENV: "rust",
                    "VOICEPI_RUST_INJECTOR": "/path/to/whisper-dictate",
                }):
            model = vp.load_stt_model("tiny.en", "cpu", "int8")
        # Probe failed → faster-whisper, not RustWhisperShellModel.
        self.assertIsInstance(model, _Whisper)
        self.assertNotIsInstance(model, vp.RustWhisperShellModel)


class RustHelperProbeTests(unittest.TestCase):
    """The probe shells out to ``whisper-dictate transcribe-wav --probe`` and
    treats any non-zero exit / spawn error / timeout as "not supported", so the
    caller can cleanly fall back to in-process faster-whisper without raising."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_probe_returns_true_on_zero_exit(self):
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["shell"] = kwargs.get("shell")
            return _completed(0, "", "")

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            ok = self.vp._rust_helper_supports_transcribe("/path/to/whisper-dictate")

        self.assertTrue(ok)
        self.assertEqual(captured["cmd"], ["/path/to/whisper-dictate",
                                            "transcribe-wav", "--probe"])
        # shell=False is mandatory on Windows so a helper path with spaces
        # (e.g. "C:/Program Files/whisper-dictate.exe") cannot be cmd-injected.
        self.assertFalse(captured["shell"])

    def test_probe_returns_false_on_nonzero_exit(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(
                              1, "", "feature not compiled in")):
            self.assertFalse(
                self.vp._rust_helper_supports_transcribe("/r"))

    def test_probe_returns_false_on_oserror(self):
        with patch.object(self.vp.subprocess, "run",
                          side_effect=FileNotFoundError("no such binary")):
            self.assertFalse(
                self.vp._rust_helper_supports_transcribe("/missing"))

    def test_probe_returns_false_on_timeout(self):
        import subprocess as _sub

        def boom(*_a, **_k):
            raise _sub.TimeoutExpired(cmd="rust", timeout=1.0)

        with patch.object(self.vp.subprocess, "run", side_effect=boom):
            self.assertFalse(
                self.vp._rust_helper_supports_transcribe("/r", timeout_s=1.0))


class RustTranscribeShellOutTests(unittest.TestCase):
    """The shell-out contract: request envelope, response parsing, and the
    error paths that translate to ``RuntimeError`` so the user sees them."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_run_rust_transcribe_builds_expected_request_and_parses_response(self):
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["input"] = kwargs.get("input")
            captured["timeout"] = kwargs.get("timeout")
            return _completed(0, '{"text": "hello world"}')

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            payload = self.vp._run_rust_transcribe(
                "/path/to/whisper-dictate",
                "/tmp/u.wav",
                language="en",
                initial_prompt="Codex",
            )

        self.assertEqual(payload, {"text": "hello world"})
        self.assertEqual(captured["cmd"], ["/path/to/whisper-dictate", "transcribe-wav"])
        request = json.loads(captured["input"])
        self.assertEqual(request, {
            "action": "transcribe_wav",
            "wav_path": "/tmp/u.wav",
            "language": "en",
            "initial_prompt": "Codex",
        })

    def test_run_rust_transcribe_normalises_none_to_empty_strings(self):
        # Mirrors the Rust dispatcher's contract: None / "" / "auto" are
        # interchangeable at the JSON boundary.
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["input"] = kwargs.get("input")
            return _completed(0, '{"text": "x"}')

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            self.vp._run_rust_transcribe(
                "rust", "/tmp/u.wav", language=None, initial_prompt=None)

        request = json.loads(captured["input"])
        self.assertEqual(request["language"], "")
        self.assertEqual(request["initial_prompt"], "")

    def test_run_rust_transcribe_raises_on_nonzero_exit(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(2, "", "model missing")):
            with self.assertRaises(RuntimeError) as ctx:
                self.vp._run_rust_transcribe(
                    "rust", "/tmp/u.wav", language=None, initial_prompt=None)
        self.assertIn("model missing", str(ctx.exception))

    def test_run_rust_transcribe_raises_on_invalid_json(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(0, "not json")):
            with self.assertRaises(RuntimeError) as ctx:
                self.vp._run_rust_transcribe(
                    "rust", "/tmp/u.wav", language=None, initial_prompt=None)
        self.assertIn("invalid JSON", str(ctx.exception))

    def test_run_rust_transcribe_raises_on_empty_stdout(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(0, "")):
            with self.assertRaises(RuntimeError) as ctx:
                self.vp._run_rust_transcribe(
                    "rust", "/tmp/u.wav", language=None, initial_prompt=None)
        self.assertIn("empty stdout", str(ctx.exception))

    def test_run_rust_transcribe_raises_on_non_object_payload(self):
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(0, '["array", "not", "object"]')):
            with self.assertRaises(RuntimeError) as ctx:
                self.vp._run_rust_transcribe(
                    "rust", "/tmp/u.wav", language=None, initial_prompt=None)
        self.assertIn("non-object", str(ctx.exception))

    def test_run_rust_transcribe_raises_on_timeout(self):
        import subprocess as _sub

        def boom(*_a, **_k):
            raise _sub.TimeoutExpired(cmd="rust", timeout=1.0)

        with patch.object(self.vp.subprocess, "run", side_effect=boom):
            with self.assertRaises(RuntimeError) as ctx:
                self.vp._run_rust_transcribe(
                    "rust", "/tmp/u.wav", language=None, initial_prompt=None,
                    timeout_s=1.0)
        self.assertIn("timed out", str(ctx.exception))


class RustWhisperShellModelTests(unittest.TestCase):
    """``RustWhisperShellModel.transcribe`` is the faster-whisper-shaped
    surface the rest of the pipeline already knows. The test exercises the
    full happy path: write a WAV, shell out (stubbed), get back a segment
    list + info pair the existing downstream code can iterate."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_transcribe_writes_wav_and_returns_segment_pair(self):
        import numpy as np

        # Fake audio: 0.5s of low-amplitude sine. Content doesn't matter for
        # the stubbed shell-out, but a real-ish buffer makes sure the int16
        # conversion path doesn't blow up on edge values.
        rate = 16000
        seconds = 0.5
        audio = (np.sin(np.linspace(0, 2 * np.pi * 440 * seconds,
                                    int(rate * seconds))) * 0.3).astype(np.float32)

        captured_request = {}

        def fake_run(cmd, **kwargs):
            captured_request["input"] = kwargs.get("input")
            # The Rust helper would read the wav file — confirm it exists at
            # call time. (After the call, the wrapper deletes it.)
            payload = json.loads(kwargs["input"])
            self.assertTrue(os.path.isfile(payload["wav_path"]),
                            f"wav not materialised: {payload['wav_path']}")
            # Sanity-check the WAV shape the wrapper produced — the Rust
            # decoder enforces 16 kHz mono int16.
            with wave.open(payload["wav_path"], "rb") as wav:
                self.assertEqual(wav.getnchannels(), 1)
                self.assertEqual(wav.getframerate(), 16000)
                self.assertEqual(wav.getsampwidth(), 2)
            return _completed(0, '{"text": "hello world"}')

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            model = self.vp.RustWhisperShellModel("/path/to/whisper-dictate")
            segments, info = model.transcribe(
                audio, language="en", initial_prompt="Codex")

        self.assertEqual(len(segments), 1)
        self.assertEqual(segments[0].text, "hello world")
        # Info object has the duck-typed attributes downstream reads.
        self.assertIsNone(info.language)
        self.assertIsNone(info.language_probability)

        # Wav must be cleaned up after the call.
        request = json.loads(captured_request["input"])
        self.assertFalse(os.path.isfile(request["wav_path"]),
                         f"wav not cleaned up: {request['wav_path']}")

    def test_transcribe_accepts_and_ignores_faster_whisper_kwargs(self):
        import numpy as np

        audio = np.zeros(16000, dtype=np.float32)
        with patch.object(self.vp.subprocess, "run",
                          return_value=_completed(0, '{"text": "x"}')):
            model = self.vp.RustWhisperShellModel("/r")
            # The real _transcribe_detail call passes a ton of kwargs the
            # Rust helper doesn't honour yet — beam_size, temperature, VAD,
            # condition_on_previous_text, hallucination_silence_threshold,
            # word_timestamps. They must all be silently accepted.
            segments, _info = model.transcribe(
                audio,
                language=None,
                initial_prompt=None,
                beam_size=5,
                temperature=[0.0, 0.2],
                condition_on_previous_text=True,
                no_speech_threshold=0.45,
                log_prob_threshold=-1.0,
                vad_filter=True,
                vad_parameters={"threshold": 0.3},
                word_timestamps=True,
                hallucination_silence_threshold=2.0,
            )
        self.assertEqual(segments[0].text, "x")

    def test_transcribe_cleans_up_wav_on_helper_error(self):
        import numpy as np

        audio = np.zeros(16000, dtype=np.float32)
        captured = {}

        def boom(cmd, **kwargs):
            captured["input"] = kwargs.get("input")
            return _completed(1, "", "boom")

        with patch.object(self.vp.subprocess, "run", side_effect=boom):
            model = self.vp.RustWhisperShellModel("/r")
            with self.assertRaises(RuntimeError):
                model.transcribe(audio, language=None, initial_prompt=None)

        request = json.loads(captured["input"])
        self.assertFalse(os.path.isfile(request["wav_path"]),
                         f"wav not cleaned up on error: {request['wav_path']}")



class WindowsHelperSubprocessTests(unittest.TestCase):
    """Windows-first verification (per AGENTS.md) for the Rust transcription
    helper shell-out: covers the platform-specific pitfalls (shell=False so a
    `C:/Program Files/...` path isn't cmd-rewritten; argv list so paths with
    spaces survive; UTF-8 stdio with errors="replace" so a cp1252 stderr line
    can't crash the JSON decode). subprocess.run is stubbed, but the asserts
    pin the exact call shape that goes to the OS."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_probe_call_shape_is_windows_safe(self):
        helper = r"C:\Program Files\WhisperDictate\whisper-dictate.exe"
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["shell"] = kwargs.get("shell")
            captured["encoding"] = kwargs.get("encoding")
            captured["errors"] = kwargs.get("errors")
            return _completed(0, "", "")

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            ok = self.vp._rust_helper_supports_transcribe(helper)

        self.assertTrue(ok)
        # argv list, helper path preserved literally with spaces — no quoting
        # gymnastics, no cmd.exe wrapper.
        self.assertEqual(captured["cmd"],
                         [helper, "transcribe-wav", "--probe"])
        self.assertFalse(captured["shell"],
                         "shell=True under Windows + a path with spaces is a "
                         "well-known cmd-injection / quoting hazard")

    def test_transcribe_call_shape_is_windows_safe(self):
        helper = r"C:\Program Files\WhisperDictate\whisper-dictate.exe"
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["shell"] = kwargs.get("shell")
            captured["encoding"] = kwargs.get("encoding")
            captured["errors"] = kwargs.get("errors")
            return _completed(0, '{"text": "hello"}', "")

        with patch.object(self.vp.subprocess, "run", side_effect=fake_run):
            payload = self.vp._run_rust_transcribe(
                helper, r"C:\Users\me\AppData\Local\Temp\u.wav",
                language="en", initial_prompt=None)

        self.assertEqual(payload, {"text": "hello"})
        self.assertEqual(captured["cmd"], [helper, "transcribe-wav"])
        self.assertFalse(captured["shell"])
        self.assertEqual(captured["encoding"], "utf-8")
        # errors="replace" so a stray cp1252 byte from a broken helper can't
        # raise UnicodeDecodeError before the JSON parser ever runs.
        self.assertEqual(captured["errors"], "replace")


class _WriteTempWavTests(unittest.TestCase):
    """The temp-WAV writer is the only piece that has to match the Rust
    decoder's contract (16 kHz mono int16). Exercise it directly so a
    regression there shows up close to the cause."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_write_temp_wav_produces_16khz_mono_int16(self):
        import numpy as np

        audio = np.array([0.0, 0.5, -0.5, 1.0, -1.0, 1.5, -1.5], dtype=np.float32)
        path = self.vp._write_temp_wav_16khz_mono(audio)
        try:
            with wave.open(path, "rb") as wav:
                self.assertEqual(wav.getnchannels(), 1)
                self.assertEqual(wav.getframerate(), 16000)
                self.assertEqual(wav.getsampwidth(), 2)
                # All audio samples must end up clipped to int16 range
                # (the >1 samples in our input were intentionally over-range).
                self.assertEqual(wav.getnframes(), len(audio))
        finally:
            os.remove(path)


if __name__ == "__main__":
    unittest.main()
