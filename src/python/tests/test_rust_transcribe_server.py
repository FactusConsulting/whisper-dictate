"""Wave 8-A of #348: long-running transcribe-server tests.

Split from `test_rust_transcribe.py` per AGENTS.md modularity gate
(~500 LOC per file). Same helpers (`_completed`, `_fresh_vp_transcribe`,
`real_numpy`, `_write_hello_wav`) live in the original file, so this
suite re-imports them from there to avoid duplication. The split is
purely a file-size win — no behaviour change.
"""
from helpers import (
    json,
    os,
    patch,
    real_numpy,
    sys,
    unittest,
)

# Re-use the shared subprocess.run completed-stub from the sibling
# test module so we don't fork the fixture shape.
from test_rust_transcribe import _completed, _fresh_vp_transcribe


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
