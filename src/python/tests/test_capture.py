"""Capture start-path tests for the RECORDING loop (Dictate audio capture).

These drive the live capture plumbing with a FAKE audio backend (no real
sounddevice/arecord device touched). As in test_dictate_loop.py, a real
Dictate is built via object.__new__ and only the attributes each code path
reads are set; the OS/audio boundaries (subprocess.Popen, threading.Thread,
sounddevice) are stubbed so frames accumulate and streams stop cleanly.

Covered (teardown + metering live in test_capture_streams.py):
  - _arecord_reader: reads S16_LE chunks from a fake proc into self.frames,
    sets the first-audio event, stops when recording flips off / EOF.
  - _start_arecord: backend/device/channel selection + reader thread wiring.
  - _start_sounddevice: device-name + channel selection (extends the
    fallback test already in test_audio.py).
"""
from helpers import (
    _env,
    io,
    json,
    load_voice_pi_realnp,
    patch,
    redirect_stderr,
    types,
    unittest,
)


class _FakeStdout:
    """Minimal arecord stdout: hands back queued byte chunks then EOF."""

    def __init__(self, chunks):
        self._chunks = list(chunks)

    def read(self, _n):
        if self._chunks:
            return self._chunks.pop(0)
        return b""


class _FakeProc:
    def __init__(self, chunks):
        self.stdout = _FakeStdout(chunks)
        self.terminated = False
        self.waited = False

    def terminate(self):
        self.terminated = True

    def wait(self):
        self.waited = True


class ArecordReaderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:  # numpy missing
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # The capture methods + their globals (np, SR, _ARECORD_DEVICE,
        # subprocess, threading) live in vp_capture's CaptureMixin; drive them
        # through that module so patched globals resolve where the methods read.
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def _reader_target(self):
        np = self.np
        return types.SimpleNamespace(
            recording=True,
            frames=[],
            _first_audio_event=self.runtime.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _chunk: None,
        )

    def test_arecord_reader_accumulates_int16_frames_and_sets_first_audio(self):
        np = self.np
        # Two ~125ms chunks of S16 mono. Reader reshapes to (-1, 1).
        c0 = np.full(2000, 100, dtype=np.int16).tobytes()
        c1 = np.full(2000, 200, dtype=np.int16).tobytes()
        proc = _FakeProc([c0, c1])
        target = self._reader_target()

        self.runtime.CaptureMixin._arecord_reader(target, proc)

        self.assertEqual(len(target.frames), 2)
        self.assertEqual(target.frames[0].shape, (2000, 1))
        self.assertEqual(target.frames[0].dtype, np.int16)
        self.assertEqual(int(target.frames[0][0, 0]), 100)
        self.assertEqual(int(target.frames[1][0, 0]), 200)
        # First-audio event/time are set on the very first chunk.
        self.assertTrue(target._first_audio_event.is_set())
        self.assertGreater(target._first_audio_at, 0.0)
        self.assertEqual(target._record_started, target._first_audio_at)

    def test_arecord_reader_stops_on_eof(self):
        np = self.np
        proc = _FakeProc([np.zeros(2000, dtype=np.int16).tobytes()])
        target = self._reader_target()

        # recording stays True, but the fake proc returns b"" after one chunk,
        # so the loop must break on EOF rather than spin forever.
        self.runtime.CaptureMixin._arecord_reader(target, proc)

        self.assertEqual(len(target.frames), 1)

    def test_arecord_reader_stops_when_recording_flag_clears(self):
        np = self.np
        # Plenty of chunks queued; reader must stop after the flag clears.
        chunks = [np.zeros(2000, dtype=np.int16).tobytes() for _ in range(50)]
        proc = _FakeProc(chunks)
        target = self._reader_target()

        emitted = {"n": 0}

        def _flip(_chunk):
            emitted["n"] += 1
            if emitted["n"] >= 3:
                target.recording = False

        target._emit_audio_level = _flip

        self.runtime.CaptureMixin._arecord_reader(target, proc)

        # Loop re-checks self.recording at the top: after the 3rd emit flips
        # the flag, no further chunk is read. So exactly 3 frames.
        self.assertEqual(len(target.frames), 3)

    def test_arecord_reader_emits_audio_level_per_chunk(self):
        np = self.np
        proc = _FakeProc([np.zeros(2000, dtype=np.int16).tobytes()])
        seen = []
        target = self._reader_target()
        target._emit_audio_level = seen.append

        self.runtime.CaptureMixin._arecord_reader(target, proc)

        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0].shape, (2000, 1))


class StartArecordTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # The capture methods + their globals (np, SR, _ARECORD_DEVICE,
        # subprocess, threading) live in vp_capture's CaptureMixin; drive them
        # through that module so patched globals resolve where the methods read.
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")

    def test_start_arecord_sets_backend_device_and_spawns_reader_thread(self):
        rt = self.runtime
        np = __import__("numpy")
        chunk = np.zeros(2000, dtype=np.int16).tobytes()
        fake_proc = _FakeProc([chunk])

        popen_calls = []

        def fake_popen(cmd, **kwargs):
            popen_calls.append((cmd, kwargs))
            return fake_proc

        thread_calls = []

        class FakeThread:
            def __init__(self, target=None, args=(), daemon=None):
                thread_calls.append((target, args, daemon))
                self._target = target
                self._args = args

            def start(self):
                # Run synchronously so we can assert frames accumulate.
                self._target(*self._args)

        target = types.SimpleNamespace(
            recording=True,
            frames=[],
            _arecord_proc=None,
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _first_audio_event=rt.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _c: None,
        )
        # The reader thread target calls self._arecord_reader; bind the real
        # method so the synchronous FakeThread fills frames for real.
        target._arecord_reader = lambda proc: rt.CaptureMixin._arecord_reader(target, proc)

        with patch.object(rt, "_ARECORD_DEVICE", "pipewire"), \
                patch.object(rt.subprocess, "Popen", fake_popen), \
                patch.object(rt.threading, "Thread", FakeThread):
            backend, device = rt.CaptureMixin._start_arecord(target)

        self.assertEqual(backend, "arecord")
        self.assertEqual(device, "arecord -D pipewire")
        self.assertEqual(target._capture_backend, "arecord")
        self.assertEqual(target._capture_channels, 1)
        self.assertIs(target._arecord_proc, fake_proc)
        # arecord invoked with the expected device + S16_LE mono 16k flags.
        cmd = popen_calls[0][0]
        self.assertEqual(cmd[:3], ["arecord", "-D", "pipewire"])
        self.assertIn("S16_LE", cmd)
        self.assertIn(str(rt.SR), cmd)
        # Default (probed) device → arecord chatter is suppressed.
        self.assertIs(popen_calls[0][1]["stderr"], rt.subprocess.DEVNULL)
        # Reader thread is a daemon and (run synchronously here) filled frames.
        self.assertEqual(len(thread_calls), 1)
        self.assertTrue(thread_calls[0][2])  # daemon=True
        self.assertEqual(len(target.frames), 1)

    def test_start_arecord_custom_device_keeps_stderr_visible(self):
        # A user-configured -D value can be invalid; silencing stderr would make
        # that failure undiagnosable, so it must flow to the worker's stderr.
        rt = self.runtime
        popen_calls = []

        def fake_popen(cmd, **kwargs):
            popen_calls.append((cmd, kwargs))
            return _FakeProc([])

        target = types.SimpleNamespace(
            recording=False,
            frames=[],
            _arecord_proc=None,
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
        )
        target._arecord_reader = lambda proc: None

        with patch.object(rt, "_ARECORD_DEVICE", "pipewire"), \
                patch.object(rt.subprocess, "Popen", fake_popen), \
                _env(VOICEPI_AUDIO_DEVICE="hw:1,0"):
            rt.CaptureMixin._start_arecord(target)

        cmd, kwargs = popen_calls[0]
        self.assertEqual(cmd[:3], ["arecord", "-D", "hw:1,0"])
        self.assertIsNone(kwargs["stderr"])


class StartSounddeviceTests(unittest.TestCase):
    """Extends the low-latency-fallback test in test_audio.py with the
    happy path and the channel-candidate fallback ordering."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        # The capture methods + their globals (np, SR, _ARECORD_DEVICE,
        # subprocess, threading) live in vp_capture's CaptureMixin; drive them
        # through that module so patched globals resolve where the methods read.
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")

    def _fake_target(self):
        return types.SimpleNamespace(
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _stream=None,
            _cb=lambda *_a: None,
        )

    def test_start_sounddevice_uses_first_working_channel_count(self):
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (1, 2)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Studio Mic",
                "max_input_channels": 2,
            },
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertEqual(device, "Studio Mic")
        # max_input_channels=2 -> first candidate is 2 and it works immediately.
        self.assertEqual(target._capture_channels, 2)
        self.assertEqual(opened[0]["channels"], 2)
        self.assertTrue(target._stream.started)

    def test_start_sounddevice_falls_back_to_fewer_channels(self):
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The hardware only accepts mono; reject anything wider.
                if kwargs["channels"] != 1:
                    raise RuntimeError("invalid channel count")
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (4, 5)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Mono Cam",
                "max_input_channels": 4,
            },
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(target._capture_channels, 1)
        self.assertTrue(target._stream.started)
        # Candidates for max=4 are [4, 2, 1]; only 1 succeeds.
        self.assertEqual([k["channels"] for k in opened][-1], 1)
        self.assertIn(4, [k["channels"] for k in opened])

    def test_start_sounddevice_raises_when_all_fail(self):
        rt = self.runtime
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                raise RuntimeError("device busy")

        class Default:
            device = (0, 0)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Busy", "max_input_channels": 1},
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            with self.assertRaises(RuntimeError) as ctx:
                rt.CaptureMixin._start_sounddevice(target)
        # No explicit device resolved here (empty setting) so there is no
        # default fallback to try; the open simply fails for every combo.
        self.assertIn("could not open", str(ctx.exception))
        self.assertIsNone(target._stream)

    def test_start_sounddevice_falls_back_to_default_when_wasapi_open_fails(self):
        # A configured device resolves to an explicit index whose open RAISES
        # (WASAPI can fail on some machines). Capture must fall back to the
        # system default (device=None) rather than break dictation.
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The explicitly-resolved device (index 5) fails to open;
                # the default (no device= kwarg) succeeds.
                if kwargs.get("device") == 5:
                    raise RuntimeError("WASAPI open failed")
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (5, 0)

        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Other", "hostapi": 0, "max_input_channels": 2},
            {"name": "Other", "hostapi": 1, "max_input_channels": 2},
            {"name": "Other", "hostapi": 2, "max_input_channels": 2},
            {"name": "Filler", "hostapi": 0, "max_input_channels": 0},
            {"name": "Headset Microphone (Jabra Evolve 65 TE)", "hostapi": 2,
             "max_input_channels": 2},
        ]
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 2},
            {"name": "Windows WASAPI", "default_input_device": 5},
        ]
        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: (
                devices if device is None and kind is None
                else (devices[device] if isinstance(device, int)
                      else {"name": "default", "max_input_channels": 2})
            ),
            query_hostapis=lambda: hostapis,
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            with _env(VOICEPI_AUDIO_DEVICE="Headset Microphone (Jabra Evolve 65 TE)"):
                backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertTrue(target._stream.started)
        # The WASAPI device (index 5) was tried first and failed...
        self.assertTrue(any(k.get("device") == 5 for k in opened))
        # ...then a fallback open with NO explicit device (default) succeeded.
        self.assertTrue(any("device" not in k for k in opened))

    def test_wasapi_autoconvert_retried_before_default_fallback(self):
        # WASAPI robustness: a WASAPI device that rejects a raw 16k open must be
        # retried with WasapiSettings(auto_convert=True) BEFORE dropping to the
        # system default. Here the plain open fails but the auto_convert open
        # succeeds, so capture stays on the configured WASAPI device.
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class _WasapiSettings:
            def __init__(self, auto_convert=False):
                self.auto_convert = auto_convert

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The WASAPI device (index 5) only opens with auto_convert on.
                if kwargs.get("device") == 5 and "extra_settings" not in kwargs:
                    raise RuntimeError("16k shared-mode rejected")
                self.started = False

            def start(self):
                self.started = True

        class Default:
            device = (5, 0)

        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Other", "hostapi": 0, "max_input_channels": 2},
            {"name": "Other", "hostapi": 1, "max_input_channels": 2},
            {"name": "Other", "hostapi": 2, "max_input_channels": 2},
            {"name": "Filler", "hostapi": 0, "max_input_channels": 0},
            {"name": "Headset Microphone (Jabra Evolve 65 TE)", "hostapi": 2,
             "max_input_channels": 2},
        ]
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 2},
            {"name": "Windows WASAPI", "default_input_device": 5},
        ]
        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            WasapiSettings=_WasapiSettings,
            default=Default(),
            query_devices=lambda device=None, kind=None: (
                devices if device is None and kind is None
                else (devices[device] if isinstance(device, int)
                      else {"name": "default", "max_input_channels": 2})
            ),
            query_hostapis=lambda: hostapis,
        )
        target = self._fake_target()

        with patch.object(rt.os, "name", "nt"):
            with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
                with _env(VOICEPI_AUDIO_DEVICE="Headset Microphone (Jabra Evolve 65 TE)"):
                    backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertTrue(target._stream.started)
        # The auto_convert candidate (extra_settings set, still device=5) was the
        # one that opened — NOT a default (device-absent) fallback.
        win = [k for k in opened if k.get("device") == 5 and "extra_settings" in k]
        self.assertTrue(win)
        self.assertTrue(win[-1]["extra_settings"].auto_convert)
        self.assertFalse(any("device" not in k for k in opened))

    def test_total_failure_threads_last_error_into_message(self):
        # Diagnosability: when EVERY open fails the raised RuntimeError must
        # carry the underlying PortAudio error, not just a generic string.
        rt = self.runtime
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                raise RuntimeError("PaErrorCode -9996 device unavailable")

        class Default:
            device = (0, 0)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Busy", "max_input_channels": 1},
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            with self.assertRaises(RuntimeError) as ctx:
                rt.CaptureMixin._start_sounddevice(target)
        msg = str(ctx.exception)
        self.assertIn("could not open", msg)
        self.assertIn("PaErrorCode -9996 device unavailable", msg)


# ---------------------------------------------------------------------------
# Part A: stream.start() failure is handled like an open failure (no crash)
# ---------------------------------------------------------------------------

class StreamStartFailureTests(unittest.TestCase):
    """The Yeti repro surfaces AUDCLNT_E_UNSUPPORTED_FORMAT on stream.START(),
    not on open(). start() now runs inside the guarded open loop so a start
    failure falls through the channel/latency/native-rate fallbacks instead of
    escaping the PTT listener."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.runtime = importlib.import_module("whisper_dictate.vp_capture")

    def _fake_target(self):
        return types.SimpleNamespace(
            _capture_backend="", _audio_input_device="", _capture_channels=0,
            _stream=None, _cb=lambda *_a: None,
        )

    def test_start_failure_at_16k_falls_back_to_native_rate(self):
        # The 16k open succeeds but start() raises (WASAPI rejects 16k on start);
        # the native-rate (48k) open+start succeeds. Capture must end up on the
        # configured device at 48k, NOT crash and NOT drop to the default.
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                self._rate = kwargs["samplerate"]
                self.closed = False

            def start(self):
                if self._rate == 16000:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT PaErrorCode -9999")
                self.started = True

            def close(self):
                self.closed = True

        class Default:
            device = (5, 0)

        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Other", "hostapi": 0, "max_input_channels": 2},
            {"name": "Other", "hostapi": 1, "max_input_channels": 2},
            {"name": "Other", "hostapi": 2, "max_input_channels": 2},
            {"name": "Filler", "hostapi": 0, "max_input_channels": 0},
            {"name": "Microphone (Yeti Classic)", "hostapi": 2,
             "max_input_channels": 2, "default_samplerate": 48000.0},
        ]
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 2},
            {"name": "Windows WASAPI", "default_input_device": 5},
        ]

        def query_devices(device=None, kind=None):
            if device is None and kind is None:
                return devices
            if isinstance(device, int):
                return devices[device]
            return {"name": "default", "max_input_channels": 2,
                    "default_samplerate": 48000.0}

        fake_sd = types.SimpleNamespace(
            InputStream=Stream, default=Default(),
            query_devices=query_devices, query_hostapis=lambda: hostapis,
        )
        target = self._fake_target()

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        # Ended up at the device native rate (resampled to 16k at consumption).
        self.assertEqual(target._capture_rate, 48000)
        self.assertTrue(getattr(target._stream, "started", False))
        # A 16k stream was opened then CLOSED (start failed); a 48k stream opened.
        self.assertTrue(any(k["samplerate"] == 16000 for k in opened))
        self.assertTrue(any(k["samplerate"] == 48000 for k in opened))
        # No drop to the system default (device kwarg stayed on index 5).
        self.assertTrue(all(k.get("device") == 5 for k in opened))

    def test_default_device_16k_reject_falls_back_to_native_rate(self):
        # No explicit device: the system default rejects a 16k open but accepts
        # its native rate. Capture must succeed at the native rate (not raise).
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                if kwargs["samplerate"] == 16000:
                    raise RuntimeError("Invalid sample rate PaErrorCode -9997")

            def start(self):
                self.started = True

            def close(self):
                pass

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            default=types.SimpleNamespace(device=(2, 0)),
            query_devices=lambda device=None, kind=None: {
                "name": "Default In", "max_input_channels": 2,
                "default_samplerate": 44100.0},
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertEqual(target._capture_rate, 44100)
        self.assertTrue(getattr(target._stream, "started", False))

    def test_total_open_failure_raises_runtimeerror_not_portaudio(self):
        # When even the native-rate open fails, _start_sounddevice raises a
        # single RuntimeError (caught by Dictate._start) — it never lets the raw
        # PortAudioError escape unwrapped.
        rt = self.runtime
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                raise RuntimeError("Invalid sample rate PaErrorCode -9997")

        class Default:
            device = (0, 0)

        fake_sd = types.SimpleNamespace(
            InputStream=Stream, default=Default(),
            query_devices=lambda device=None, kind=None: {
                "name": "Busy", "max_input_channels": 1,
                "default_samplerate": 48000.0},
        )
        target = self._fake_target()

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            with self.assertRaises(RuntimeError) as ctx:
                rt.CaptureMixin._start_sounddevice(target)
        self.assertIn("could not open", str(ctx.exception))


# ---------------------------------------------------------------------------
# Part B: native-rate capture + software resample helpers
# ---------------------------------------------------------------------------

class ResampleCaptureBufferTests(unittest.TestCase):
    """_resample_capture_buffer: 48k→16k shortens the buffer to the 16k-rate
    duration and returns mono int16; 16k is a bit-identical no-op."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def test_resample_48k_to_16k_matches_duration_and_is_int16_mono(self):
        np = self.np
        self.rt.SR = 16000
        # 1.0 s captured at 48k → 48000 samples in, ~16000 samples out.
        pcm = np.zeros((48000, 1), dtype=np.int16)
        out = self.rt._resample_capture_buffer(pcm, 48000)
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(out.ndim, 2)
        self.assertEqual(out.shape[1], 1)
        # Within one sample of the 16k-rate duration (1.0 s → 16000 samples).
        self.assertTrue(abs(out.shape[0] - 16000) <= 1,
                        f"expected ~16000 samples, got {out.shape[0]}")

    def test_resample_16k_native_is_bit_identical_noop(self):
        np = self.np
        self.rt.SR = 16000
        rng = np.random.default_rng(0)
        pcm = rng.integers(-3000, 3000, size=8000, dtype=np.int16).reshape(-1, 1)
        out = self.rt._resample_capture_buffer(pcm, 16000)
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(out.shape, (8000, 1))
        # No resampling occurred — values are bit-identical to the input.
        self.assertTrue(np.array_equal(out.reshape(-1), pcm.reshape(-1)))

    def test_resample_preserves_a_tone_frequency(self):
        # A 440 Hz tone captured at 48k must still be ~440 Hz after the 16k
        # resample (sanity check that we're resampling, not just truncating).
        np = self.np
        self.rt.SR = 16000
        t = np.arange(48000) / 48000.0
        tone = (np.sin(2 * np.pi * 440 * t) * 20000).astype(np.int16).reshape(-1, 1)
        out = self.rt._resample_capture_buffer(tone, 48000).reshape(-1)
        freqs = np.fft.rfftfreq(len(out), d=1.0 / 16000)
        peak = freqs[int(np.argmax(np.abs(np.fft.rfft(out.astype(np.float32)))))]
        self.assertTrue(abs(peak - 440) < 15, f"peak freq {peak} Hz off from 440")


class NativeRateOpenTests(unittest.TestCase):
    """_open_native_rate_stream picks the device default rate when 16k fails."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")

    def test_chosen_capture_rate_is_device_default(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)

            def start(self):
                self.started = True

            def close(self):
                pass

        fake_sd = types.SimpleNamespace(
            InputStream=Stream,
            query_devices=lambda device=None, kind=None: {
                "name": "Yeti", "max_input_channels": 2,
                "default_samplerate": 44100.0},
            default=types.SimpleNamespace(device=(3, 0)),
        )

        stream, channels, rate, exc = rt._open_native_rate_stream(
            fake_sd, 3, lambda *_a: None)
        self.assertIsNotNone(stream)
        self.assertEqual(rate, 44100)
        self.assertIsNone(exc)
        self.assertTrue(all(k["samplerate"] == 44100 for k in opened))

    def test_no_op_when_native_rate_equals_sr(self):
        rt = self.rt
        rt.SR = 16000
        fake_sd = types.SimpleNamespace(
            InputStream=lambda **k: (_ for _ in ()).throw(AssertionError("should not open")),
            query_devices=lambda device=None, kind=None: {
                "name": "Jabra", "max_input_channels": 1,
                "default_samplerate": 16000.0},
            default=types.SimpleNamespace(device=(1, 0)),
        )
        stream, channels, rate, exc = rt._open_native_rate_stream(
            fake_sd, 1, lambda *_a: None)
        self.assertIsNone(stream)
        self.assertEqual(rate, 0)
        self.assertIsNone(exc)


# ---------------------------------------------------------------------------
# Fix 1+2+3: stop-stream robustness tests
# ---------------------------------------------------------------------------

class _FakeProcTimeout:
    """arecord stub that hangs on wait() once, then succeeds on kill()/wait()."""

    def __init__(self, chunks, *, hang_first_wait=True):
        self.stdout = _FakeStdout(chunks)
        self.terminated = False
        self.killed = False
        self._wait_calls = 0
        self._hang_first = hang_first_wait

    def terminate(self):
        self.terminated = True

    def kill(self):
        self.killed = True

    def wait(self, timeout=None):
        self._wait_calls += 1
        if self._hang_first and self._wait_calls == 1:
            import subprocess as _sp
            raise _sp.TimeoutExpired("arecord", timeout)


class _FakeProcRaisingTerminate:
    """arecord stub whose terminate() raises an exception."""

    def __init__(self):
        self.stdout = _FakeStdout([])
        self._null = None

    def terminate(self):
        raise OSError("terminate failed")

    def wait(self, timeout=None):
        pass


class StopCaptureStreamsTests(unittest.TestCase):
    """Tests for Fix 1 (timeout→kill), Fix 2 (drain), Fix 3 (always None)."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def _target(self, proc, stream=None):
        return types.SimpleNamespace(
            frames=[],
            _arecord_proc=proc,
            _stream=stream,
        )

    def test_wait_timeout_triggers_kill(self):
        """Fix 1: when wait() times out, kill() is called and refs are cleared."""
        proc = _FakeProcTimeout([], hang_first_wait=True)
        target = self._target(proc)

        self.rt.CaptureMixin._stop_capture_streams(target)

        self.assertTrue(proc.terminated)
        self.assertTrue(proc.killed)
        self.assertIsNone(target._arecord_proc)

    def test_drain_appends_whole_samples_drops_odd_byte(self):
        """Fix 2: trailing bytes from stdout are drained; odd trailing byte dropped."""
        np = self.np
        # 5 int16 samples = 10 bytes, so the trailing odd byte (1 extra) is dropped.
        trailing = np.array([10, 20, 30, 40, 50], dtype=np.int16).tobytes() + b"\xff"
        # _FakeStdout returns chunks sequentially; we need it to return all at once.
        # Override stdout to return the trailing bytes as a single read() call.
        class _OneShot:
            def __init__(self, data):
                self._data = data
                self._read = False
            def read(self, n=-1):
                if not self._read:
                    self._read = True
                    return self._data
                return b""

        proc = _FakeProcTimeout([], hang_first_wait=False)
        proc.stdout = _OneShot(trailing)
        target = self._target(proc)

        self.rt.CaptureMixin._stop_capture_streams(target)

        self.assertEqual(len(target.frames), 1)
        self.assertEqual(target.frames[0].shape, (5, 1))
        self.assertEqual(target.frames[0].dtype, np.int16)
        self.assertEqual(list(target.frames[0][:, 0]), [10, 20, 30, 40, 50])
        self.assertIsNone(target._arecord_proc)

    def test_drain_empty_stdout_appends_nothing(self):
        """Fix 2: empty stdout after terminate leaves frames unchanged."""
        proc = _FakeProcTimeout([], hang_first_wait=False)
        target = self._target(proc)

        self.rt.CaptureMixin._stop_capture_streams(target)

        self.assertEqual(target.frames, [])
        self.assertIsNone(target._arecord_proc)

    def test_cleanup_sets_refs_to_none_even_when_terminate_raises(self):
        """Fix 3: refs are always cleared even when terminate() raises."""
        proc = _FakeProcRaisingTerminate()
        target = self._target(proc)

        # Should not propagate the OSError
        self.rt.CaptureMixin._stop_capture_streams(target)

        self.assertIsNone(target._arecord_proc)
        self.assertIsNone(target._stream)

    def test_stream_ref_cleared_even_when_stream_stop_raises(self):
        """Fix 3: _stream is always set to None even if stream.stop() raises."""
        class _RaisingStream:
            def stop(self):
                raise RuntimeError("stream error")
            def close(self):
                pass

        target = types.SimpleNamespace(
            frames=[],
            _arecord_proc=None,
            _stream=_RaisingStream(),
        )

        self.rt.CaptureMixin._stop_capture_streams(target)

        self.assertIsNone(target._stream)


# ---------------------------------------------------------------------------
# Fix 4: capture_lost event on arecord EOF-while-recording
# ---------------------------------------------------------------------------

class ArecordReaderCaptureLostTests(unittest.TestCase):
    """Fix 4: reader emits capture_lost when arecord EOF arrives mid-recording."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def _reader_target(self):
        return types.SimpleNamespace(
            recording=True,
            frames=[],
            _first_audio_event=self.rt.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _chunk: None,
            _cap_warned=False,
        )

    def test_eof_while_recording_emits_capture_lost(self):
        """Immediate EOF with recording=True → capture_lost event fired once."""
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        proc = _FakeProc([])  # empty → immediate EOF
        target = self._reader_target()

        with patch.object(self.rt, "_emit_worker_event", fake_emit):
            self.rt.CaptureMixin._arecord_reader(target, proc)

        capture_lost = [e for e in events if e.get("state") == "capture_lost"]
        self.assertEqual(len(capture_lost), 1,
                         f"expected exactly one capture_lost event; got {events!r}")
        self.assertIn("reason", capture_lost[0])

    def test_eof_after_recording_stopped_does_not_emit_capture_lost(self):
        """EOF after recording flag cleared → no capture_lost (normal stop)."""
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        np = self.np
        chunks = [np.zeros(2000, dtype=np.int16).tobytes()]
        proc = _FakeProc(chunks)
        target = self._reader_target()

        # Flip recording=False after the first (and only) chunk so EOF on the
        # next read is the normal end-of-recording case, not a device failure.
        def _flip(_chunk):
            target.recording = False

        target._emit_audio_level = _flip

        with patch.object(self.rt, "_emit_worker_event", fake_emit):
            self.rt.CaptureMixin._arecord_reader(target, proc)

        capture_lost = [e for e in events if e.get("state") == "capture_lost"]
        self.assertEqual(capture_lost, [],
                         f"no capture_lost expected on normal stop; got {events!r}")

    def test_reader_exception_emits_capture_lost(self):
        """Exception in the reader loop → capture_lost event with reason."""
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        class _BrokenStdout:
            def read(self, _n):
                raise IOError("device unplugged")

        class _BrokenProc:
            def __init__(self):
                self.stdout = _BrokenStdout()

        target = self._reader_target()

        with patch.object(self.rt, "_emit_worker_event", fake_emit):
            self.rt.CaptureMixin._arecord_reader(target, _BrokenProc())

        capture_lost = [e for e in events if e.get("state") == "capture_lost"]
        self.assertEqual(len(capture_lost), 1,
                         f"expected exactly one capture_lost event; got {events!r}")
        self.assertIn("device unplugged", capture_lost[0].get("reason", ""))


# ---------------------------------------------------------------------------
# Fix 5: max recording cap
# ---------------------------------------------------------------------------

class MaxRecordCapTests(unittest.TestCase):
    """Fix 5: cap stops appending frames and emits exactly one warning event."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")
        import numpy as np
        cls.np = np

    def _reader_target(self, cap_warned=False):
        return types.SimpleNamespace(
            recording=True,
            frames=[],
            _first_audio_event=self.rt.threading.Event(),
            _first_audio_at=0.0,
            _record_started=0.0,
            _emit_audio_level=lambda _chunk: None,
            _cap_warned=cap_warned,
        )

    def test_cap_stops_appending_and_emits_once(self):
        """When buffer exceeds cap, no more frames are appended and one warning fires."""
        np = self.np
        rt = self.rt
        # SR=16000; 2000 samples = 0.125s. Set cap to 0.1s so any chunk exceeds it.
        rt.SR = 16000
        # Three chunks worth of audio.
        chunks = [np.zeros(2000, dtype=np.int16).tobytes() for _ in range(3)]
        proc = _FakeProc(chunks)
        target = self._reader_target()

        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt, "_emit_worker_event", fake_emit), \
                _env(VOICEPI_MAX_RECORD_S="0.1"):
            rt.CaptureMixin._arecord_reader(target, proc)

        # The cap should have triggered after the very first chunk.
        # Frames must be empty (cap hit immediately, nothing appended).
        self.assertEqual(len(target.frames), 0,
                         f"no frames should be appended past cap; got {len(target.frames)}")
        # Exactly one cap warning event (state=recording, capped=True).
        cap_events = [e for e in events if e.get("capped") is True]
        self.assertEqual(len(cap_events), 1,
                         f"expected exactly one cap event; got {events!r}")

    def test_cap_disabled_when_zero(self):
        """VOICEPI_MAX_RECORD_S=0 disables the cap entirely."""
        np = self.np
        rt = self.rt
        rt.SR = 16000
        chunks = [np.zeros(2000, dtype=np.int16).tobytes() for _ in range(3)]
        proc = _FakeProc(chunks)
        target = self._reader_target()

        with _env(VOICEPI_MAX_RECORD_S="0"):
            rt.CaptureMixin._arecord_reader(target, proc)

        self.assertEqual(len(target.frames), 3)

    def test_cap_already_warned_does_not_emit_again(self):
        """If _cap_warned is already True, no further cap events are emitted."""
        np = self.np
        rt = self.rt
        rt.SR = 16000
        chunks = [np.zeros(2000, dtype=np.int16).tobytes() for _ in range(3)]
        proc = _FakeProc(chunks)
        # Pre-set _cap_warned so the reader knows it already fired.
        target = self._reader_target(cap_warned=True)

        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt, "_emit_worker_event", fake_emit), \
                _env(VOICEPI_MAX_RECORD_S="0.1"):
            rt.CaptureMixin._arecord_reader(target, proc)

        cap_events = [e for e in events if e.get("capped") is True]
        self.assertEqual(cap_events, [],
                         f"no cap event expected when already warned; got {events!r}")

