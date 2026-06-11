"""Trace-diagnostics tests for the capture path (maximal ``Trace`` level).

When ``VOICEPI_TRACE`` is on, the worker must make a mic that won't open
self-diagnosing from the log alone:

  - ``_open_sounddevice_stream`` logs ONE ``[trace][cap] attempt …`` line per
    candidate (host-API × samplerate × channels × dtype × auto-convert) carrying
    the per-attempt result (``ok`` or the exact exception), plus the finally
    ``BOUND`` candidate — and emits NONE of those lines when trace is off (the
    existing Basic/Verbose output is byte-for-byte unchanged).
  - ``trace_dump_audio_devices`` enumerates every input device at startup and
    NEVER raises, even when ``query_devices`` blows up.

These drive the real module-level helpers with a FAKE sounddevice (stubbed
InputStream / query_devices / query_hostapis), mirroring test_capture.py.
"""
from helpers import (
    io,
    redirect_stderr,
    types,
    unittest,
)


def _runtime():
    """The vp_capture module with its lazy numpy/SR globals populated."""
    import importlib
    rt = importlib.import_module("whisper_dictate.vp_capture")
    rt._load_runtime_modules()
    return rt


class _Stream:
    """Fake InputStream that fails int16 and succeeds float32 (a Yeti-style
    WASAPI device whose shared mixformat is float32)."""

    def __init__(self, **kwargs):
        self.kwargs = kwargs
        if kwargs.get("dtype") == "int16":
            raise RuntimeError("AUDCLNT_E_UNSUPPORTED_FORMAT")
        self.started = False

    def start(self):
        self.started = True


def _fake_sd(stream_cls=_Stream, *, name="Yeti", host="Windows WASAPI",
             channels=2, default_sr=48000.0):
    hostapis = [{"name": host}]

    def query_devices(device=None, kind=None):
        return {
            "name": name,
            "max_input_channels": channels,
            "hostapi": 0,
            "default_samplerate": default_sr,
        }

    return types.SimpleNamespace(
        InputStream=stream_cls,
        query_devices=query_devices,
        query_hostapis=lambda: hostapis,
        default=types.SimpleNamespace(device=(7, 7)),
    )


class OpenStreamTraceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.rt = _runtime()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def _open(self, trace):
        rt = self.rt
        rt.SR = 16000
        sd = _fake_sd()
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            stream, channels, dtype, exc = rt._open_sounddevice_stream(
                sd, 51, lambda *_a: None, trace=trace)
        return stream, channels, dtype, exc, stderr.getvalue()

    def test_trace_on_logs_a_line_per_attempt_with_host_rate_dtype(self):
        stream, _channels, dtype, exc, out = self._open(trace=True)

        # It still bound on a float32 candidate (behaviour is additive).
        self.assertIsNotNone(stream)
        self.assertEqual(dtype, "float32")
        self.assertIsNone(exc)

        attempts = [ln for ln in out.splitlines()
                    if ln.startswith("[trace][cap] attempt")]
        # int16 candidates were tried (and failed) BEFORE float32 succeeded, so
        # there is more than one attempt line — not just the last error.
        self.assertGreater(len(attempts), 1)
        # Every attempt line carries the full negotiation detail.
        for ln in attempts:
            self.assertIn("host=Windows WASAPI", ln)
            self.assertIn("dev=51", ln)
            self.assertIn("rate=16000", ln)
            self.assertIn("ch=", ln)
            self.assertIn("dtype=", ln)
            self.assertIn("autoconv=0", ln)
        # The exact per-candidate failure is surfaced (not swallowed).
        self.assertTrue(any("dtype=int16" in ln and "AUDCLNT_E_UNSUPPORTED_FORMAT" in ln
                            for ln in attempts))
        # A float32 attempt reports ok, and the bound candidate is logged.
        self.assertTrue(any("dtype=float32" in ln and ln.endswith("-> ok")
                            for ln in attempts))
        self.assertTrue(any(ln.startswith("[trace][cap] BOUND") and "dtype=float32" in ln
                            for ln in out.splitlines()))

    def test_trace_off_emits_no_trace_lines(self):
        stream, _channels, dtype, exc, out = self._open(trace=False)

        # Same successful bind — the float32 fallback still works.
        self.assertIsNotNone(stream)
        self.assertEqual(dtype, "float32")
        self.assertIsNone(exc)
        # No new [trace] output at all when trace is disabled.
        self.assertNotIn("[trace]", out)


class TraceDumpAudioDevicesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.rt = _runtime()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def test_dump_lists_every_input_device_when_trace_on(self):
        rt = self.rt

        def query_devices(device=None, kind=None):
            return [
                {"name": "Speakers", "max_input_channels": 0, "hostapi": 0,
                 "default_samplerate": 48000.0},
                {"name": "Yeti", "max_input_channels": 2, "hostapi": 1,
                 "default_samplerate": 48000.0},
            ]

        fake_sd = types.SimpleNamespace(
            query_devices=query_devices,
            query_hostapis=lambda: [{"name": "MME"}, {"name": "Windows WASAPI"}],
        )
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            from unittest.mock import patch
            with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
                rt.trace_dump_audio_devices()
        out = stderr.getvalue()

        self.assertIn("[trace][devices] host-apis:", out)
        # Output-only device (0 input channels) is skipped; the input mic is
        # listed with index, name, host-API, channels and native rate.
        self.assertNotIn("name='Speakers'", out)
        self.assertIn("dev=1", out)
        self.assertIn("name='Yeti'", out)
        self.assertIn("host=Windows WASAPI", out)
        self.assertIn("max_in_ch=2", out)
        self.assertIn("default_sr=48000.0", out)

    def test_dump_never_raises_on_query_failure(self):
        rt = self.rt

        def boom(device=None, kind=None):
            raise RuntimeError("portaudio exploded")

        fake_sd = types.SimpleNamespace(
            query_devices=boom,
            query_hostapis=lambda: (_ for _ in ()).throw(RuntimeError("nope")),
        )
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            from unittest.mock import patch
            with patch.dict(rt.sys.modules, {"sounddevice": fake_sd}):
                # Must NOT raise — a broken audio stack can't block startup.
                rt.trace_dump_audio_devices()
        # The failure is reported (and swallowed), not propagated.
        self.assertIn("[trace][devices]", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
