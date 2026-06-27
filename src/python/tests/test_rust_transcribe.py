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


# -----------------------------------------------------------------------
# Wave 8-A of #348: long-running `transcribe-server` wrapper.
# -----------------------------------------------------------------------


class _FakeServerProcess:
    """Stand-in for ``subprocess.Popen`` that scripts stdout responses and
    records stdin writes — the same shape ``RustWhisperServerModel`` needs
    so the live IPC contract can be tested without a real Rust binary."""

    def __init__(self, scripted_lines, *, alive_after_each=True):
        # scripted_lines is consumed left-to-right: each call to
        # stdout.readline() pops the next line. When exhausted, readline
        # returns "" (EOF), mimicking a dead child.
        self._lines = list(scripted_lines)
        self._stdin_writes = []
        self._returncode = None
        self._alive = True
        self._alive_after_each = alive_after_each
        self.stdin = self._Stdin(self)
        self.stdout = self._Stdout(self)
        self.stderr = self._Stderr()

    def writes(self):
        return list(self._stdin_writes)

    def poll(self):
        return self._returncode

    def wait(self, timeout=None):
        self._returncode = 0
        return 0

    def kill(self):
        self._returncode = -9

    def die(self, returncode=1):
        """Simulate a crashed server: drain remaining scripted lines and
        make subsequent reads return EOF."""
        self._lines.clear()
        self._alive = False
        self._returncode = returncode

    class _Stdin:
        def __init__(self, parent):
            self._parent = parent
            self._closed = False

        def write(self, data):
            if self._closed or not self._parent._alive:
                raise BrokenPipeError("fake server stdin closed")
            self._parent._stdin_writes.append(data)
            return len(data)

        def flush(self):
            pass

        def close(self):
            self._closed = True

    class _Stdout:
        def __init__(self, parent):
            self._parent = parent

        def readline(self):
            if not self._parent._lines:
                return ""
            return self._parent._lines.pop(0)

    class _Stderr:
        def read(self, n=-1):
            return ""


def _ready_line(*, model_path="/tmp/ggml-tiny.en.bin", idle_unload_s=0):
    return json.dumps({
        "ready": True,
        "model_path": model_path,
        "idle_unload_s": idle_unload_s,
    }) + "\n"


def _ok_line(text):
    return json.dumps({"text": text}) + "\n"


def _err_line(msg):
    return json.dumps({"error": msg}) + "\n"


class RustWhisperServerModelTests(unittest.TestCase):
    """End-to-end tests for the long-running ``transcribe-server`` wrapper.

    The Wave 8-A promise is "spawn once, transcribe many" — the tests
    confirm that the wrapper reads the ready handshake exactly once and
    reuses the live subprocess for every transcribe call, instead of
    paying the model-load cost per utterance the way ShellModel does.
    """

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def _build(self, fake: _FakeServerProcess):
        with patch.object(self.vp.subprocess, "Popen", return_value=fake):
            return self.vp.RustWhisperServerModel("/path/to/whisper-dictate")

    def test_ready_handshake_parses_and_exposes_config(self):
        fake = _FakeServerProcess([
            _ready_line(model_path="/m/ggml-small.en.bin", idle_unload_s=300),
        ])
        model = self._build(fake)
        self.assertEqual(model.ready["model_path"], "/m/ggml-small.en.bin")
        self.assertEqual(model.ready["idle_unload_s"], 300)
        # No stdin writes during construction — the handshake is read-only.
        self.assertEqual(fake.writes(), [])

    def test_transcribe_round_trips_a_single_request(self):
        import numpy as np

        fake = _FakeServerProcess([
            _ready_line(),
            _ok_line("hello world"),
        ])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)

        segments, info = model.transcribe(audio, language="da", initial_prompt="Codex")

        self.assertEqual(len(segments), 1)
        self.assertEqual(segments[0].text, "hello world")
        # Info object has the duck-typed attributes downstream reads.
        self.assertIsNone(info.language)

        # Exactly one JSON request on stdin, well-formed.
        writes = fake.writes()
        self.assertEqual(len(writes), 1)
        self.assertTrue(writes[0].endswith("\n"))
        req = json.loads(writes[0])
        self.assertEqual(req["action"], "transcribe_wav")
        self.assertEqual(req["language"], "da")
        self.assertEqual(req["initial_prompt"], "Codex")
        # Wav path is materialised at request time and cleaned up after.
        self.assertFalse(os.path.isfile(req["wav_path"]),
                         f"wav not cleaned: {req['wav_path']}")

    def test_two_consecutive_transcribes_reuse_one_subprocess(self):
        """The headline Wave 8-A promise: model loads ONCE, stays resident
        between calls. A second transcribe must not spawn a new subprocess
        nor re-read the ready handshake."""
        import numpy as np

        fake = _FakeServerProcess([
            _ready_line(),
            _ok_line("first"),
            _ok_line("second"),
        ])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)

        seg1, _ = model.transcribe(audio, language=None, initial_prompt=None)
        seg2, _ = model.transcribe(audio, language=None, initial_prompt=None)

        self.assertEqual(seg1[0].text, "first")
        self.assertEqual(seg2[0].text, "second")
        # Two writes — one per call. No extra ready-line read.
        self.assertEqual(len(fake.writes()), 2)

    def test_transcribe_raises_on_per_request_error_envelope(self):
        import numpy as np

        fake = _FakeServerProcess([
            _ready_line(),
            _err_line("model file vanished"),
        ])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)
        with self.assertRaises(RuntimeError) as cm:
            model.transcribe(audio, language=None, initial_prompt=None)
        self.assertIn("model file vanished", str(cm.exception))

    def test_transcribe_raises_on_unexpected_eof_mid_call(self):
        """If the server dies after the ready line but before responding,
        the wrapper must surface a clear error so the caller can re-spawn."""
        import numpy as np

        fake = _FakeServerProcess([
            _ready_line(),
            # No response line scripted — readline returns "" → EOF.
        ])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)
        with self.assertRaises(RuntimeError) as cm:
            model.transcribe(audio, language=None, initial_prompt=None)
        self.assertIn("exited or timed out mid-call", str(cm.exception))

    def test_constructor_raises_when_server_dies_before_ready(self):
        # No lines scripted → first readline returns "" → ready handshake
        # fails. The wrapper must propagate the error so the caller falls
        # back to ShellModel / faster-whisper.
        fake = _FakeServerProcess([])
        with patch.object(self.vp.subprocess, "Popen", return_value=fake):
            with self.assertRaises(RuntimeError) as cm:
                self.vp.RustWhisperServerModel("/r")
        self.assertIn("ready line", str(cm.exception))

    def test_constructor_raises_on_invalid_ready_json(self):
        fake = _FakeServerProcess(["not json at all\n"])
        with patch.object(self.vp.subprocess, "Popen", return_value=fake):
            with self.assertRaises(RuntimeError) as cm:
                self.vp.RustWhisperServerModel("/r")
        self.assertIn("valid JSON", str(cm.exception))

    def test_constructor_raises_when_ready_payload_missing_flag(self):
        # Valid JSON but no `ready: true` — wrong shape, must error.
        fake = _FakeServerProcess([json.dumps({"hello": "world"}) + "\n"])
        with patch.object(self.vp.subprocess, "Popen", return_value=fake):
            with self.assertRaises(RuntimeError) as cm:
                self.vp.RustWhisperServerModel("/r")
        self.assertIn("ready=true", str(cm.exception))

    def test_transcribe_cleans_up_wav_on_response_error(self):
        """The wav temp file must be removed even when the server returns
        an error envelope — otherwise long sessions leak /tmp space."""
        import numpy as np

        fake = _FakeServerProcess([
            _ready_line(),
            _err_line("nope"),
        ])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)
        with self.assertRaises(RuntimeError):
            model.transcribe(audio, language=None, initial_prompt=None)
        wav_path = json.loads(fake.writes()[0])["wav_path"]
        self.assertFalse(os.path.isfile(wav_path),
                         f"wav leaked on error: {wav_path}")

    def test_transcribe_accepts_and_ignores_faster_whisper_kwargs(self):
        """Same surface guarantee as ShellModel — _transcribe_detail passes
        a ton of faster-whisper kwargs the Rust path doesn't honour yet;
        they must be silently accepted so the calling code stays uniform."""
        import numpy as np

        fake = _FakeServerProcess([_ready_line(), _ok_line("x")])
        model = self._build(fake)
        audio = np.zeros(16000, dtype=np.float32)
        segments, _ = model.transcribe(
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

    def test_close_drains_pipe_and_waits_for_child(self):
        fake = _FakeServerProcess([_ready_line()])
        model = self._build(fake)
        # Sanity: child reports alive before close.
        self.assertIsNone(fake.poll())
        model.close()
        # Wait was called → returncode set.
        self.assertEqual(fake.poll(), 0)

    def test_close_is_idempotent(self):
        fake = _FakeServerProcess([_ready_line()])
        model = self._build(fake)
        model.close()
        model.close()  # must not raise

    # -- P2 #395 follow-ups: timeout, respawn, Windows-specific shape ----

    def test_transcribe_respawns_helper_after_a_per_call_death(self):
        """Headline Codex P2 fix: a single EOF used to leave self._proc
        permanently dead and every later utterance failed. The helper
        must respawn on the NEXT transcribe call so the user pays one
        failed utterance and dictation keeps working."""
        import numpy as np

        # First subprocess: ready, then dies mid-call (no response line).
        dead = _FakeServerProcess([_ready_line()])
        # Second subprocess (respawn): ready, then a real response.
        alive = _FakeServerProcess([_ready_line(), _ok_line("recovered")])
        popen_returns = iter([dead, alive])
        audio = np.zeros(16000, dtype=np.float32)

        with patch.object(self.vp.subprocess, "Popen",
                          side_effect=lambda *a, **kw: next(popen_returns)):
            model = self.vp.RustWhisperServerModel("/r")
            # First call: dead subprocess -> RuntimeError.
            with self.assertRaises(RuntimeError):
                model.transcribe(audio, language=None, initial_prompt=None)
            # Second call: must respawn the helper and succeed.
            segments, _info = model.transcribe(audio, language=None,
                                               initial_prompt=None)
            self.assertEqual(segments[0].text, "recovered")
        # And the second subprocess was actually used (one stdin write to
        # the respawned helper, not piling up on the dead one).
        self.assertEqual(len(alive.writes()), 1)

    def test_transcribe_respawn_falls_back_to_shell_when_replacement_also_dies(self):
        """If the respawn itself can't get past the ready handshake,
        ensure_alive raises so the caller sees the failure and can fall
        back to ShellModel / faster-whisper rather than getting stuck in
        a tight respawn-loop."""
        import numpy as np

        dead1 = _FakeServerProcess([_ready_line()])
        dead2 = _FakeServerProcess([])  # respawn can't even read ready
        popen_returns = iter([dead1, dead2])
        audio = np.zeros(16000, dtype=np.float32)

        with patch.object(self.vp.subprocess, "Popen",
                          side_effect=lambda *a, **kw: next(popen_returns)):
            model = self.vp.RustWhisperServerModel("/r")
            # First call: dies mid-response -> RuntimeError + mark dead.
            with self.assertRaises(RuntimeError):
                model.transcribe(audio, language=None, initial_prompt=None)
            # Second call: respawn ALSO fails -> RuntimeError surfaces.
            with self.assertRaises(RuntimeError):
                model.transcribe(audio, language=None, initial_prompt=None)

    def test_response_timeout_kills_subprocess_and_marks_dead(self):
        """A wedged whisper.cpp call must NOT hold TRANSCRIBE_LOCK
        indefinitely — the wrapper kills the subprocess after the
        configured deadline and surfaces the failure so the caller can
        log + respawn."""
        import numpy as np

        # Fake that never returns a response line (its readline() blocks
        # forever in the reader thread). The wrapper's join(timeout=...)
        # fires, the wrapper kills the proc, _read_with_timeout returns "".
        class _BlockingProcess(_FakeServerProcess):
            def __init__(self):
                super().__init__([_ready_line()])
                self._kill_called = False
                self.stdout = self._BlockingStdout(self)

            def kill(self):
                self._kill_called = True
                # Simulate kill: drain any pending lines so subsequent
                # reads return EOF immediately.
                self.stdout._wake.set()
                super().kill()

            class _BlockingStdout:
                def __init__(self, parent):
                    self._parent = parent
                    import threading as _t
                    self._wake = _t.Event()
                    # Serve the ready line once; subsequent reads block
                    # until _wake (set by kill()) and then return EOF.
                    self._first = True

                def readline(self):
                    if self._first:
                        self._first = False
                        return _ready_line()
                    self._wake.wait()
                    return ""

        fake = _BlockingProcess()
        audio = np.zeros(16000, dtype=np.float32)
        with patch.object(self.vp.subprocess, "Popen", return_value=fake):
            model = self.vp.RustWhisperServerModel("/r")
            # Pin a very short timeout for the test — 0.5 s is well above
            # the cost of writing one stdin line and joining the reader
            # thread, but bounded enough that the test runs in well under
            # a second.
            model._response_timeout_s = 0.5
            with self.assertRaises(RuntimeError) as cm:
                model.transcribe(audio, language=None, initial_prompt=None)
        # Kill was invoked (so a real subprocess WOULD have been killed).
        self.assertTrue(fake._kill_called,
                        "wrapper must kill the subprocess on timeout")
        # And the failure message names the timeout explicitly so the
        # caller can distinguish it from other RuntimeError shapes.
        self.assertIn("timed out", str(cm.exception))

    def test_stderr_is_devnull_not_pipe(self):
        """Per Codex P2 on #395: stderr MUST NOT be PIPE because we never
        drain it; a non-empty PIPE eventually fills the OS pipe buffer
        (~64 KB on Linux/Windows) and the helper blocks writing stderr,
        deadlocking the stdout response we're waiting on."""
        import numpy as np

        seen_kwargs = {}

        def capture_popen(cmd, **kwargs):
            seen_kwargs.update(kwargs)
            return _FakeServerProcess([_ready_line()])

        with patch.object(self.vp.subprocess, "Popen", side_effect=capture_popen):
            self.vp.RustWhisperServerModel("/r")
        self.assertIs(
            seen_kwargs.get("stderr"),
            self.vp.subprocess.DEVNULL,
            f"stderr must be DEVNULL (got {seen_kwargs.get('stderr')!r}); "
            "PIPE without a drain thread deadlocks the response path"
        )

    def test_popen_uses_cross_platform_safe_call_shape(self):
        """Per Codex P2 on #395 (Windows-coverage rule from AGENTS.md):
        the long-running server's subprocess.Popen call must use the
        same defensive options the per-utterance ShellModel uses —
        ``shell=False`` (no shell injection), ``text=True`` +
        ``encoding="utf-8"`` (Danish corpus text survives the
        cp1252 Windows console), ``errors="replace"`` (a stray byte
        never tears down the pipe). The helper path is also passed
        as an args LIST (not a concatenated string) so a path with
        spaces (the default Windows install at
        ``C:/Program Files/WhisperDictate/whisper-dictate.exe``)
        round-trips correctly."""
        seen_args = {}

        def capture_popen(cmd, **kwargs):
            seen_args["cmd"] = cmd
            seen_args["kwargs"] = kwargs
            return _FakeServerProcess([_ready_line()])

        helper_with_spaces = "C:\\Program Files\\WhisperDictate\\whisper-dictate.exe"
        with patch.object(self.vp.subprocess, "Popen", side_effect=capture_popen):
            self.vp.RustWhisperServerModel(helper_with_spaces)

        # Helper path passed as args LIST element (not concatenated).
        self.assertIsInstance(seen_args["cmd"], list,
                              "cmd must be a list, not a shell string")
        self.assertEqual(seen_args["cmd"][0], helper_with_spaces,
                         "helper path must be the first list element")
        self.assertEqual(seen_args["cmd"][1], "transcribe-server",
                         "subcommand must be the second list element")
        # Cross-platform call-shape defaults.
        self.assertEqual(seen_args["kwargs"].get("shell"), False,
                         "shell=False so a path with spaces isn't parsed")
        self.assertEqual(seen_args["kwargs"].get("text"), True,
                         "text=True so we get str not bytes")
        self.assertEqual(seen_args["kwargs"].get("encoding"), "utf-8",
                         "UTF-8 for Danish corpus text on Windows cp1252")
        self.assertEqual(seen_args["kwargs"].get("errors"), "replace",
                         "errors=replace so a stray byte doesn't crash readline")


class LoadStTModelPrefersServerWhenAvailableTests(unittest.TestCase):
    """``load_stt_model`` wiring: when ``VOICEPI_TRANSCRIBE_BACKEND=rust`` is
    set and the helper supports the long-running mode, the server is
    preferred over the per-utterance ShellModel. When the server handshake
    fails (e.g. older binary without the subcommand) we fall back to
    ShellModel rather than skipping the Rust path entirely."""

    def setUp(self):
        sys.modules["numpy"] = real_numpy()
        self.vp = _fresh_vp_transcribe()

    def test_load_stt_model_returns_server_when_handshake_succeeds(self):
        fake = _FakeServerProcess([
            _ready_line(model_path="/m/ggml.bin", idle_unload_s=120),
        ])
        with patch.object(self.vp, "STT_BACKEND", "whisper"), \
                patch.object(self.vp, "_assert_local_backend", lambda *_a, **_k: None), \
                patch.object(self.vp, "_rust_helper_supports_transcribe",
                             return_value=True), \
                patch.object(self.vp.subprocess, "Popen", return_value=fake), \
                patch.dict(os.environ, {
                    self.vp.TRANSCRIBE_BACKEND_ENV: "rust",
                    "VOICEPI_RUST_INJECTOR": "/path/to/whisper-dictate",
                }):
            model = self.vp.load_stt_model("base", "cpu", "int8")
        self.assertIsInstance(model, self.vp.RustWhisperServerModel)
        self.assertEqual(model.ready["model_path"], "/m/ggml.bin")

    def test_load_stt_model_falls_back_to_shell_when_server_handshake_fails(self):
        # No ready line → server constructor raises → caller falls back to
        # ShellModel. The user gets the per-utterance path instead of
        # losing dictation entirely.
        fake = _FakeServerProcess([])
        with patch.object(self.vp, "STT_BACKEND", "whisper"), \
                patch.object(self.vp, "_assert_local_backend", lambda *_a, **_k: None), \
                patch.object(self.vp, "_rust_helper_supports_transcribe",
                             return_value=True), \
                patch.object(self.vp.subprocess, "Popen", return_value=fake), \
                patch.dict(os.environ, {
                    self.vp.TRANSCRIBE_BACKEND_ENV: "rust",
                    "VOICEPI_RUST_INJECTOR": "/path/to/whisper-dictate",
                }):
            model = self.vp.load_stt_model("base", "cpu", "int8")
        self.assertIsInstance(model, self.vp.RustWhisperShellModel)


@unittest.skipUnless(sys.platform == "win32",
                     "Windows-specific subprocess invocation test")
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
