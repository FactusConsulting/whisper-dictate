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
        rt = self.runtime
        target = types.SimpleNamespace(
            _capture_backend="",
            _audio_input_device="",
            _capture_channels=0,
            _capture_dtype="int16",
            _capture_rate=16000,
            _stream=None,
            _cb=lambda *_a: None,
        )
        # _start_sounddevice calls self._bind_stream / self._note_device_swap;
        # bind the real mixin methods so the namespace target drives them.
        target._bind_stream = (
            lambda *a, **k: rt.CaptureMixin._bind_stream(target, *a, **k))
        target._note_device_swap = (
            lambda *a, **k: rt.CaptureMixin._note_device_swap(target, *a, **k))
        target._try_sibling_endpoints = (
            lambda *a, **k: rt.CaptureMixin._try_sibling_endpoints(target, *a, **k))
        return target

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

    def test_start_sounddevice_explicit_device_unusable_raises_no_default_swap(self):
        # Fix 1: a configured (EXPLICIT) device resolves to an index whose open
        # RAISES on every host API and has NO sibling endpoint. Capture must NOT
        # silently record from the system default (a DIFFERENT physical mic) —
        # it raises DeviceUnusableError so the worker can surface an honest error.
        rt = self.runtime
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The explicitly-resolved device (index 5) fails to open;
                # the default (no device= kwarg) WOULD succeed — but must not be
                # reached for an explicit selection.
                if kwargs.get("device") == 5:
                    raise RuntimeError("WASAPI open failed")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

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
                with self.assertRaises(rt.DeviceUnusableError) as ctx:
                    rt.CaptureMixin._start_sounddevice(target)

        # The WASAPI device (index 5) was tried and failed...
        self.assertTrue(any(k.get("device") == 5 for k in opened))
        # ...and NO default open was attempted (no wrong-mic swap).
        self.assertFalse(any("device" not in k for k in opened))
        # The error names the chosen device + the actionable next step.
        self.assertIn("Jabra", str(ctx.exception))
        self.assertIn("select a different microphone", str(ctx.exception).lower())
        self.assertIn("Jabra", ctx.exception.device_label)

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
        rt = self.runtime
        target = types.SimpleNamespace(
            _capture_backend="", _audio_input_device="", _capture_channels=0,
            _capture_dtype="int16", _capture_rate=16000,
            _stream=None, _cb=lambda *_a: None,
        )
        target._bind_stream = (
            lambda *a, **k: rt.CaptureMixin._bind_stream(target, *a, **k))
        target._note_device_swap = (
            lambda *a, **k: rt.CaptureMixin._note_device_swap(target, *a, **k))
        target._try_sibling_endpoints = (
            lambda *a, **k: rt.CaptureMixin._try_sibling_endpoints(target, *a, **k))
        return target

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

    def test_resample_fast_path_int16_does_not_copy(self):
        # Regression: the no-resample fast path used `.astype(np.int16)` which
        # always copies. For an int16 (N, 1) input at 16k the result must be
        # int16 (N, 1) AND share memory with the input (no needless copy).
        np = self.np
        self.rt.SR = 16000
        pcm = np.arange(4000, dtype=np.int16).reshape(-1, 1)
        out = self.rt._resample_capture_buffer(pcm, 16000)
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(out.shape, (4000, 1))
        self.assertTrue(
            np.shares_memory(out, pcm),
            "int16 fast path must not copy the buffer")

    def test_resample_fast_path_falsy_rate_returns_int16_n1(self):
        # capture_rate falsy (0/None) → fast path, int16 (N, 1) shape contract.
        np = self.np
        self.rt.SR = 16000
        pcm = np.arange(2000, dtype=np.int16)
        out = self.rt._resample_capture_buffer(pcm, 0)
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(out.shape, (2000, 1))
        self.assertTrue(np.shares_memory(out, pcm))

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

        stream, channels, dtype, rate, exc = rt._open_native_rate_stream(
            fake_sd, 3, lambda *_a: None)
        self.assertIsNotNone(stream)
        self.assertEqual(rate, 44100)
        self.assertEqual(dtype, "int16")
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
        stream, channels, dtype, rate, exc = rt._open_native_rate_stream(
            fake_sd, 1, lambda *_a: None)
        self.assertIsNone(stream)
        self.assertEqual(rate, 0)
        self.assertEqual(dtype, "")
        self.assertIsNone(exc)

    def test_inputstream_raises_before_bind_returns_error_cleanly(self):
        # Regression: when InputStream(**kwargs) raises BEFORE `stream` is bound,
        # the except-block cleanup must NOT hit an UnboundLocalError calling
        # stream.close(). It returns (None, 0, "", last_error) carrying the real
        # PortAudio error, and never attempts a close on the unbound stream.
        rt = self.rt
        rt.SR = 16000
        close_calls = {"n": 0}

        class _RaisingInputStream:
            def __init__(self, **_kwargs):
                # Construction itself fails — `stream` never gets bound.
                raise RuntimeError("PaErrorCode -9996 device unavailable")

            def close(self):
                close_calls["n"] += 1

        fake_sd = types.SimpleNamespace(
            InputStream=_RaisingInputStream,
            query_devices=lambda device=None, kind=None: {
                "name": "Dead Mic", "max_input_channels": 1},
            default=types.SimpleNamespace(device=(1, 0)),
        )
        stream, channels, dtype, exc = rt._open_sounddevice_stream(
            fake_sd, 1, lambda *_a: None)
        self.assertIsNone(stream)
        self.assertEqual(channels, 0)
        self.assertEqual(dtype, "")
        self.assertIsInstance(exc, RuntimeError)
        self.assertIn("device unavailable", str(exc))
        # close() must never be called on an unbound stream.
        self.assertEqual(close_calls["n"], 0)


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


# ---------------------------------------------------------------------------
# Format negotiation: int16 → float32 dtype fallback (Blue Yeti repro)
# ---------------------------------------------------------------------------

def _bound_target(rt):
    """A namespace 'self' for _start_sounddevice with the mixin methods bound."""
    target = types.SimpleNamespace(
        _capture_backend="", _audio_input_device="", _capture_channels=0,
        _capture_dtype="int16", _capture_rate=16000, _stream=None,
        _cb=lambda *_a: None,
    )
    target._bind_stream = lambda *a, **k: rt.CaptureMixin._bind_stream(target, *a, **k)
    target._note_device_swap = (
        lambda *a, **k: rt.CaptureMixin._note_device_swap(target, *a, **k))
    target._try_sibling_endpoints = (
        lambda *a, **k: rt.CaptureMixin._try_sibling_endpoints(target, *a, **k))
    return target


def _yeti_fake_sd(stream_cls, *, default_samplerate=48000.0, default_input=5):
    """A fake sounddevice exposing a WASAPI Yeti at index 5 (48k float32).

    ``default_input`` is the system-default INPUT device index used when capture
    falls back to ``device=None`` (set it to a NON-Yeti index to exercise the
    device-swap honesty path, where the default resolves to a different mic).
    Index 1 is a separate "Jabra-like" input below.
    """
    devices = [
        {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
        {"name": "Headset Microphone (Jabra Evolve 65 TE)", "hostapi": 2,
         "max_input_channels": 2, "default_samplerate": 16000.0},
        {"name": "Other", "hostapi": 1, "max_input_channels": 2},
        {"name": "Other", "hostapi": 2, "max_input_channels": 2},
        {"name": "Filler", "hostapi": 0, "max_input_channels": 0},
        {"name": "Microphone (Yeti Classic)", "hostapi": 2,
         "max_input_channels": 2, "default_samplerate": default_samplerate},
    ]
    hostapis = [
        {"name": "MME", "default_input_device": 0},
        {"name": "Windows DirectSound", "default_input_device": default_input},
        {"name": "Windows WASAPI", "default_input_device": default_input},
    ]

    def query_devices(device=None, kind=None):
        if device is None and kind is None:
            return devices
        if isinstance(device, int):
            return devices[device]
        return {"name": devices[default_input]["name"], "max_input_channels": 2,
                "default_samplerate": default_samplerate}

    return types.SimpleNamespace(
        InputStream=stream_cls,
        default=types.SimpleNamespace(device=(default_input, 0)),
        query_devices=query_devices, query_hostapis=lambda: hostapis)


class Float32FallbackTests(unittest.TestCase):
    """A device that rejects int16 but accepts float32 must bind on float32 and
    NOT swap to the system default; the callback converts float32 → int16."""

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

    def test_int16_rejected_float32_accepted_binds_float32_no_swap(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # int16 is rejected at any rate (Yeti shared mixformat=float32);
                # float32 at 16k succeeds so no native-rate / default fallback.
                if kwargs["dtype"] == "int16":
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT PaErrorCode -9999")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_fake_sd(Stream)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertTrue(getattr(target._stream, "started", False))
        # Bound on the float32 candidate, at 16k (no native-rate needed)...
        self.assertEqual(target._capture_dtype, "float32")
        self.assertEqual(target._capture_rate, 16000)
        # ...on the CONFIGURED device (index 5), never dropping to the default.
        self.assertTrue(all(k.get("device") == 5 for k in opened))
        # int16 was attempted (and exhausted) before float32 on the same device.
        self.assertTrue(any(k["dtype"] == "int16" for k in opened))
        # No device-swap WARN: still the Yeti.
        self.assertNotIn("WARN", target._audio_input_device)
        self.assertIn("Yeti", target._audio_input_device)

    def test_int16_first_for_native_16k_device_is_unchanged_happy_path(self):
        # REGRESSION: a 16k int16 device (Jabra-like) binds on the FIRST candidate
        # (int16, max channels, low latency) with NO float32 / native attempt.
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Headset Microphone (Jabra Evolve 65 TE)", "hostapi": 2,
             "max_input_channels": 2, "default_samplerate": 16000.0},
        ]
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 1},
            {"name": "Windows WASAPI", "default_input_device": 1},
        ]
        fake_sd = types.SimpleNamespace(
            InputStream=Stream, default=types.SimpleNamespace(device=(1, 0)),
            query_devices=lambda device=None, kind=None: (
                devices if device is None and kind is None
                else (devices[device] if isinstance(device, int)
                      else {"name": "default", "max_input_channels": 2,
                            "default_samplerate": 16000.0})),
            query_hostapis=lambda: hostapis)
        target = _bound_target(rt)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Headset Microphone (Jabra Evolve 65 TE)"):
            rt.CaptureMixin._start_sounddevice(target)

        # Exactly ONE open: the very first candidate worked.
        self.assertEqual(len(opened), 1)
        first = opened[0]
        self.assertEqual(first["dtype"], "int16")
        self.assertEqual(first["samplerate"], 16000)
        self.assertEqual(first["channels"], 2)  # native max channels
        self.assertEqual(first["latency"], "low")  # low-latency candidate
        self.assertEqual(target._capture_dtype, "int16")
        self.assertEqual(target._capture_rate, 16000)
        self.assertNotIn("WARN", target._audio_input_device)

    def test_native_rate_float32_resamples_to_16k_mono_int16(self):
        # Yeti at 48000/float32: int16 rejected at every rate, float32 rejected at
        # 16k, float32 accepted at the native 48k → buffer resampled to 16k.
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                self._dtype = kwargs["dtype"]
                self._rate = kwargs["samplerate"]
                if self._dtype == "int16":
                    raise RuntimeError("int16 unsupported")
                if self._rate == 16000:  # float32 @16k still rejected
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_fake_sd(Stream)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(target._capture_dtype, "float32")
        self.assertEqual(target._capture_rate, 48000)
        self.assertTrue(all(k.get("device") == 5 for k in opened))
        self.assertNotIn("WARN", target._audio_input_device)

        # The float32-frame → int16 → resample chain produces mono int16 at 16k.
        np = self.np
        frame_48k = np.full((48000, 1), 0.25, dtype=np.float32)
        chunk = rt._capture_frame_to_int16(frame_48k, target._capture_dtype)
        self.assertEqual(chunk.dtype, np.int16)
        out = rt._resample_capture_buffer(chunk, target._capture_rate)
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(out.shape[1], 1)
        self.assertTrue(abs(out.shape[0] - 16000) <= 1)


class CaptureFrameToInt16Tests(unittest.TestCase):
    """float32 → int16 conversion correctness, including ±1.0 clipping."""

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

    def test_float32_frame_converts_to_expected_int16_with_clipping(self):
        np = self.np
        frame = np.array([[0.0], [0.5], [-0.5], [1.0], [-1.0], [2.0], [-3.0]],
                         dtype=np.float32)
        out = self.rt._capture_frame_to_int16(frame, "float32")
        self.assertEqual(out.dtype, np.int16)
        # 0.5*32767=16383 (trunc), 1.0→32767, clip 2.0→1.0→32767, -3.0→-1.0→-32767.
        self.assertEqual(list(out[:, 0]), [0, 16383, -16383, 32767, -32767, 32767, -32767])

    def test_int16_frame_is_bit_identical_copy(self):
        np = self.np
        frame = np.array([[10], [-20], [30000], [-30000]], dtype=np.int16)
        out = self.rt._capture_frame_to_int16(frame, "int16")
        self.assertEqual(out.dtype, np.int16)
        self.assertTrue(np.array_equal(out, frame))
        self.assertIsNot(out, frame)  # a copy, not the same object

    def test_float32_dtype_inferred_from_array_when_dtype_arg_int16(self):
        # Defensive: even if the tracked dtype says int16, a float32 array frame
        # is still converted (never stored raw float into the int16 buffer).
        np = self.np
        frame = np.full((4, 1), 0.5, dtype=np.float32)
        out = self.rt._capture_frame_to_int16(frame, "int16")
        self.assertEqual(out.dtype, np.int16)
        self.assertEqual(list(out[:, 0]), [16383, 16383, 16383, 16383])


class DeviceSwapHonestyTests(unittest.TestCase):
    """Fix 1: when an EXPLICITLY-chosen device fails ALL formats and has no
    working sibling endpoint, capture must NOT silently record from a DIFFERENT
    default device — it raises DeviceUnusableError instead of swapping mics."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")

    def test_explicit_device_failing_all_formats_raises_not_swaps(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The Yeti (index 5) fails EVERY format/rate; the default
                # (device kwarg absent) WOULD succeed — but must NOT be reached
                # for an explicit selection (no wrong-mic swap).
                if kwargs.get("device") == 5:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        # The system default resolves to a DIFFERENT mic (the Jabra at index 1).
        fake_sd = _yeti_fake_sd(Stream, default_input=1)
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt.os, "name", "nt"), \
                patch.object(rt, "_emit_worker_event", fake_emit), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_WORKER_EVENTS="1",
                     VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            with self.assertRaises(rt.DeviceUnusableError) as ctx:
                rt.CaptureMixin._start_sounddevice(target)

        # The Yeti (index 5) was tried and failed every format...
        self.assertTrue(any(k.get("device") == 5 for k in opened))
        # ...and NO default open (no device= kwarg) was ever attempted.
        self.assertFalse(any("device" not in k for k in opened))
        # The error names the chosen device + the actionable next step; NO
        # wrong-mic swap event was emitted.
        self.assertIn("Yeti", str(ctx.exception))
        self.assertIn("select a different microphone", str(ctx.exception).lower())
        self.assertEqual([e for e in events if "device_swap" in e], [])

    def test_no_warn_when_no_explicit_device_was_chosen(self):
        # An empty selection (system default) that needs a native-rate fallback is
        # NOT a swap — nothing the user chose was abandoned, so no WARN.
        rt = self.rt
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                if kwargs["samplerate"] == 16000:
                    raise RuntimeError("Invalid sample rate PaErrorCode -9997")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        fake_sd = types.SimpleNamespace(
            InputStream=Stream, default=types.SimpleNamespace(device=(2, 0)),
            query_devices=lambda device=None, kind=None: {
                "name": "Default In", "max_input_channels": 2,
                "default_samplerate": 44100.0})
        target = _bound_target(rt)

        with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
            rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(target._capture_rate, 44100)
        self.assertNotIn("WARN", target._audio_input_device)

    def test_no_explicit_device_still_falls_back_to_system_default(self):
        # Fix 1 must NOT change the no-explicit-device path: with an EMPTY
        # VOICEPI_AUDIO_DEVICE, the resolved preferred-host-API default endpoint
        # (index 5) fails every format, but the system default (device=None)
        # opens — so capture binds it WITHOUT raising and WITHOUT a swap WARN
        # (nothing the user chose was abandoned).
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The resolved default endpoint (index 5) fails; the bare default
                # (no device= kwarg) succeeds.
                if kwargs.get("device") == 5:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_fake_sd(Stream, default_input=5)
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt.os, "name", "nt"), \
                patch.object(rt, "_emit_worker_event", fake_emit), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_WORKER_EVENTS="1", VOICEPI_AUDIO_DEVICE=""):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertTrue(getattr(target._stream, "started", False))
        # The system-default open (no device= kwarg) is what bound.
        self.assertTrue(any("device" not in k for k in opened))
        # No honest-error, no wrong-mic swap WARN: it is the default by design.
        self.assertNotIn("WARN", target._audio_input_device)
        self.assertEqual([e for e in events if "device_swap" in e], [])


# ---------------------------------------------------------------------------
# Host-API sibling fallback (the core fix): a Yeti whose WASAPI endpoint won't
# open is recorded via the SAME physical device's DirectSound / MME endpoint
# rather than silently swapping to a DIFFERENT mic.
# ---------------------------------------------------------------------------

# The verified Blue Yeti Classic table: ONE physical mic exposed on three host
# APIs. host API order: 0=MME, 1=DirectSound, 2=WASAPI, 3=WDM-KS.
_YETI_FULL = "Microphone (Yeti Classic)"           # WASAPI / DirectSound full name
_YETI_MME = "Microphone (Yeti Classic"             # MME 31-char-ish truncation


def _yeti_hostapi_sd(stream_cls, *, jabra_default=False):
    """Fake sounddevice exposing the Yeti on MME(7)/DirectSound(25)/WASAPI(51).

    Index layout mirrors the probe: the WASAPI endpoint (51) is the one the app
    resolves; its DirectSound (25) and MME (7, truncated name) siblings are the
    SAME physical mic. ``jabra_default=True`` points the system default at a
    DIFFERENT mic (the Jabra at index 1) so the device-swap honesty path can be
    exercised when EVERY Yeti endpoint fails.
    """
    devices = [
        {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},          # 0
        {"name": "Headset Microphone (Jabra Evolve 65 TE)", "hostapi": 2,
         "max_input_channels": 2, "default_samplerate": 16000.0},             # 1 WASAPI Jabra
        {"name": "Sound Mapper - Input", "hostapi": 0, "max_input_channels": 2},  # 2
    ]
    # Pad so the stated indices line up with the probe (7=MME, 25=DS, 51=WASAPI).
    def _pad(target_index):
        while len(devices) < target_index:
            devices.append({"name": "Filler", "hostapi": 0, "max_input_channels": 0})
    _pad(7)
    devices.append({"name": _YETI_MME, "hostapi": 0, "max_input_channels": 2,
                    "default_samplerate": 44100.0})                            # 7 MME Yeti
    _pad(25)
    devices.append({"name": _YETI_FULL, "hostapi": 1, "max_input_channels": 2,
                    "default_samplerate": 44100.0})                            # 25 DS Yeti
    _pad(51)
    devices.append({"name": _YETI_FULL, "hostapi": 2, "max_input_channels": 2,
                    "default_samplerate": 48000.0})                            # 51 WASAPI Yeti

    default_input = 1 if jabra_default else 51
    hostapis = [
        {"name": "MME", "default_input_device": 7},
        {"name": "Windows DirectSound", "default_input_device": 25},
        {"name": "Windows WASAPI", "default_input_device": default_input},
    ]

    def query_devices(device=None, kind=None):
        if device is None and kind is None:
            return devices
        if isinstance(device, int):
            return devices[device]
        return devices[default_input]

    return types.SimpleNamespace(
        InputStream=stream_cls,
        default=types.SimpleNamespace(device=(default_input, 0)),
        query_devices=query_devices, query_hostapis=lambda: hostapis)


class SiblingEndpointFallbackTests(unittest.TestCase):
    """When the WASAPI endpoint of the chosen mic fails every format, capture
    must open the SAME physical device via its DirectSound (then MME) endpoint
    BEFORE falling back to a different mic — with NO WasapiSettings and no
    resample when 16k int16 is accepted directly."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")

    def test_wasapi_fails_directsound_sibling_binds_16k_no_swap_no_resample(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The WASAPI endpoint (51) rejects EVERY format/rate/dtype; the
                # DirectSound sibling (25) accepts 16k int16 directly.
                if kwargs.get("device") == 51:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT PaErrorCode -9999")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            backend, device = rt.CaptureMixin._start_sounddevice(target)

        self.assertEqual(backend, "sounddevice")
        self.assertTrue(getattr(target._stream, "started", False))
        # Bound on the DirectSound sibling (index 25), at 16k int16, no resample.
        self.assertEqual(target._capture_rate, 16000)
        self.assertEqual(target._capture_dtype, "int16")
        bound = opened[-1]
        self.assertEqual(bound["device"], 25)
        self.assertEqual(bound["dtype"], "int16")
        self.assertEqual(bound["samplerate"], 16000)
        # NO swap to a different physical device: still the Yeti, no WARN.
        self.assertNotIn("WARN", target._audio_input_device)
        self.assertIn("Yeti", target._audio_input_device)
        # The MME endpoint (7) was NOT needed (DirectSound won first).
        self.assertFalse(any(k.get("device") == 7 for k in opened))

    def test_wasapi_settings_never_passed_to_sibling_endpoints(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class _WasapiSettings:
            def __init__(self, auto_convert=False):
                self.auto_convert = auto_convert

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # WASAPI (51) rejects everything (even auto_convert); the
                # DirectSound sibling (25) accepts 16k int16.
                if kwargs.get("device") == 51:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream)
        fake_sd.WasapiSettings = _WasapiSettings

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            rt.CaptureMixin._start_sounddevice(target)

        # The non-WASAPI sibling endpoints (DirectSound=25, MME=7) must NEVER be
        # opened with extra_settings (WasapiSettings) — that is -9984 on MME/DS.
        sibling_opens = [k for k in opened if k.get("device") in (25, 7)]
        self.assertTrue(sibling_opens, "expected at least one sibling open")
        self.assertTrue(all("extra_settings" not in k for k in sibling_opens),
                        f"WasapiSettings leaked to a non-WASAPI endpoint: {sibling_opens!r}")
        # auto_convert WAS attempted on the WASAPI endpoint itself (device 51).
        self.assertTrue(any(k.get("device") == 51 and "extra_settings" in k
                            for k in opened))

    def test_mme_sibling_binds_when_directsound_also_fails(self):
        # WASAPI (51) AND DirectSound (25) fail every format; the MME sibling (7,
        # truncated name) still opens 16k int16 → bound on MME, still the Yeti.
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                if kwargs.get("device") in (51, 25):
                    raise RuntimeError("format rejected")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            rt.CaptureMixin._start_sounddevice(target)

        self.assertTrue(getattr(target._stream, "started", False))
        bound = opened[-1]
        self.assertEqual(bound["device"], 7)  # MME endpoint
        self.assertNotIn("WARN", target._audio_input_device)
        self.assertIn("Yeti", target._audio_input_device)
        # DirectSound (25) was tried before MME (7).
        ds_first = next(i for i, k in enumerate(opened) if k.get("device") == 25)
        mme_first = next(i for i, k in enumerate(opened) if k.get("device") == 7)
        self.assertLess(ds_first, mme_first)

    def test_all_host_api_endpoints_fail_raises_no_swap(self):
        # Fix 1: WASAPI + DirectSound + MME endpoints of the EXPLICITLY-chosen
        # Yeti ALL fail. Capture must NOT drop to the system default (a DIFFERENT
        # mic, the Jabra) — every endpoint of the chosen device was exhausted, so
        # it raises DeviceUnusableError instead of silently swapping mics.
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # Every Yeti endpoint (51 WASAPI, 25 DS, 7 MME) fails; the default
                # (no device= kwarg) WOULD succeed — but must not be reached.
                if kwargs.get("device") in (51, 25, 7):
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream, jabra_default=True)
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt.os, "name", "nt"), \
                patch.object(rt, "_emit_worker_event", fake_emit), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_WORKER_EVENTS="1",
                     VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            with self.assertRaises(rt.DeviceUnusableError) as ctx:
                rt.CaptureMixin._start_sounddevice(target)

        # All three Yeti host-API endpoints were attempted (full sibling sweep)...
        self.assertTrue(any(k.get("device") == 51 for k in opened))
        self.assertTrue(any(k.get("device") == 25 for k in opened))
        self.assertTrue(any(k.get("device") == 7 for k in opened))
        # ...and NO default open (no device= kwarg) was ever attempted.
        self.assertFalse(any("device" not in k for k in opened))
        # No wrong-mic swap event; the error names the chosen device + next step.
        self.assertEqual([e for e in events if "device_swap" in e], [])
        self.assertIn("Yeti", str(ctx.exception))
        self.assertIn("select a different microphone", str(ctx.exception).lower())

    def test_no_swap_warn_when_sibling_succeeds(self):
        # The negative of the above: when a sibling endpoint binds, the WARN must
        # NOT fire (it is the SAME physical device, not a swap) and NO device-swap
        # event is emitted.
        rt = self.rt
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                if kwargs.get("device") == 51:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream, jabra_default=True)
        events = []

        def fake_emit(event_type, **kwargs):
            events.append({"type": event_type, **kwargs})

        with patch.object(rt.os, "name", "nt"), \
                patch.object(rt, "_emit_worker_event", fake_emit), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_WORKER_EVENTS="1",
                     VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            rt.CaptureMixin._start_sounddevice(target)

        self.assertNotIn("WARN", target._audio_input_device)
        self.assertEqual([e for e in events if "device_swap" in e], [])

    def test_trace_logs_sibling_fallback_attempt_with_host_api(self):
        # With VOICEPI_TRACE on, the host-API sibling fallback must be self-
        # diagnosing: a [trace][cap] sibling-fallback line names the host API and
        # the sibling endpoint index it is about to try.
        rt = self.rt
        rt.SR = 16000

        class Stream:
            def __init__(self, **kwargs):
                if kwargs.get("device") == 51:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        target = _bound_target(rt)
        fake_sd = _yeti_hostapi_sd(Stream)

        stderr = io.StringIO()
        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_TRACE="1",
                     VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"), \
                redirect_stderr(stderr):
            rt.CaptureMixin._start_sounddevice(target)

        out = stderr.getvalue()
        sibling_lines = [ln for ln in out.splitlines()
                         if ln.startswith("[trace][cap] sibling-fallback")]
        self.assertTrue(sibling_lines, f"expected a sibling-fallback trace line; got {out!r}")
        # The DirectSound endpoint (25) is named with its host API.
        self.assertTrue(any("host=Windows DirectSound" in ln and "dev=25" in ln
                            for ln in sibling_lines))


class BaseLatencyCandidateTests(unittest.TestCase):
    """The low-latency WASAPI regression fix: the default-latency / default-
    blocksize ('base') candidate is ALWAYS attempted, so a device that opens
    only WITHOUT latency='low' still binds (the probe proves base works)."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.runtime = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        cls.runtime._load_runtime_modules()
        import importlib
        cls.rt = importlib.import_module("whisper_dictate.vp_capture")

    def test_base_candidate_reached_when_low_latency_rejected(self):
        rt = self.rt
        rt.SR = 16000
        opened = []

        class Stream:
            def __init__(self, **kwargs):
                opened.append(kwargs)
                # The device rejects the explicit low-latency blocksize but
                # accepts the default-latency / default-blocksize base candidate.
                if kwargs.get("latency") == "low" or "blocksize" in kwargs:
                    raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT (low latency)")
                self.started = False

            def start(self):
                self.started = True

            def close(self):
                pass

        devices = [
            {"name": "Speakers", "hostapi": 0, "max_input_channels": 0},
            {"name": "Microphone (Yeti Classic)", "hostapi": 2,
             "max_input_channels": 2, "default_samplerate": 16000.0},
        ]
        hostapis = [
            {"name": "MME", "default_input_device": 0},
            {"name": "Windows DirectSound", "default_input_device": 1},
            {"name": "Windows WASAPI", "default_input_device": 1},
        ]
        fake_sd = types.SimpleNamespace(
            InputStream=Stream, default=types.SimpleNamespace(device=(1, 0)),
            query_devices=lambda device=None, kind=None: (
                devices if device is None and kind is None
                else (devices[device] if isinstance(device, int)
                      else {"name": "default", "max_input_channels": 2,
                            "default_samplerate": 16000.0})),
            query_hostapis=lambda: hostapis)
        target = _bound_target(rt)

        with patch.object(rt.os, "name", "nt"), \
                patch.dict(rt.sys.modules, {"sounddevice": fake_sd}), \
                _env(VOICEPI_AUDIO_DEVICE="Microphone (Yeti Classic)"):
            rt.CaptureMixin._start_sounddevice(target)

        self.assertTrue(getattr(target._stream, "started", False))
        # A low-latency candidate was tried (and failed)...
        self.assertTrue(any(k.get("latency") == "low" for k in opened))
        # ...and the base candidate (no blocksize, no latency='low') was reached
        # and is the one that bound — still the configured device, no swap.
        bound = opened[-1]
        self.assertNotIn("blocksize", bound)
        self.assertNotEqual(bound.get("latency"), "low")
        self.assertNotIn("WARN", target._audio_input_device)
        self.assertEqual(target._capture_rate, 16000)

