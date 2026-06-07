"""Unit tests for vp_events (extracted from runtime.py).

Covers the event/emission primitives and live-capture support helpers that the
push-to-talk loop and the file/calibration paths share: compact-text/base-event
shaping, worker-event gating + ASCII encoding, sounddevice device probing and
capture-stream kwargs, audio metering, and the Rust command-hook / profile /
model-capacity bridges (with the helper subprocess stubbed).
"""
from helpers import (
    _capture_stdout,
    _env,
    io,
    json,
    real_numpy,
    patch,
    redirect_stderr,
    types,
    unittest,
)

from whisper_dictate import vp_events


class CompactTextTests(unittest.TestCase):
    def test_compact_text_collapses_whitespace(self):
        self.assertEqual(vp_events._compact_text("  a\t b\n c "), "a b c")

    def test_compact_text_truncates_with_ellipsis(self):
        out = vp_events._compact_text("x" * 300, limit=10)
        self.assertEqual(len(out), 10)
        self.assertTrue(out.endswith("..."))

    def test_compact_text_keeps_short_text_verbatim(self):
        self.assertEqual(vp_events._compact_text("short", limit=240), "short")


class BaseEventTests(unittest.TestCase):
    def test_base_event_stamps_ts_and_merges_fields(self):
        event = vp_events._base_event(event="utterance", text="hi")
        self.assertEqual(event["event"], "utterance")
        self.assertEqual(event["text"], "hi")
        self.assertIn("ts", event)
        self.assertIsInstance(event["ts"], float)

    def test_emit_json_writes_single_sorted_object(self):
        with _capture_stdout() as buf:
            vp_events._emit_json({"b": 2, "a": 1})
        self.assertEqual(buf.getvalue().strip(), '{"a": 1, "b": 2}')


class EmitWorkerEventTests(unittest.TestCase):
    def test_worker_event_is_silent_without_opt_in(self):
        with _env(VOICEPI_WORKER_EVENTS=None):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                vp_events._emit_worker_event("audio", level=0.5)
        self.assertEqual(stderr.getvalue(), "")

    def test_worker_event_drops_none_fields_and_ascii_encodes(self):
        with _env(VOICEPI_WORKER_EVENTS="1"):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                vp_events._emit_worker_event(
                    "audio", state="recording", device="Mikrofon æ", skip=None)
        line = stderr.getvalue().strip()
        self.assertTrue(line.startswith("[worker-event] "))
        escaped = "\\u%04x" % ord("æ")  # ascii-safe escape on the wire
        self.assertIn(escaped, line)         # non-ASCII escaped on the wire...
        self.assertNotIn("æ", line)          # ...never emitted raw
        payload = json.loads(line.removeprefix("[worker-event] "))
        self.assertEqual(payload["event"], "audio")
        self.assertEqual(payload["device"], "Mikrofon æ")
        self.assertNotIn("skip", payload)  # None fields dropped


class SounddeviceProbeTests(unittest.TestCase):
    def _sd(self, default, info):
        return types.SimpleNamespace(
            default=types.SimpleNamespace(device=default),
            query_devices=lambda device=None, kind=None: info,
        )

    def test_input_name_uses_default_input_index(self):
        sd = self._sd((3, 7), {"name": "USB Mic", "max_input_channels": 2})
        self.assertEqual(vp_events._sounddevice_input_name(sd), "USB Mic")
        self.assertEqual(vp_events._sounddevice_input_channels(sd), 2)

    def test_input_name_handles_missing_info(self):
        sd = types.SimpleNamespace(
            default=types.SimpleNamespace(device=None),
            query_devices=lambda device=None, kind=None: None,
        )
        self.assertIsNone(vp_events._sounddevice_input_name(sd))
        self.assertEqual(vp_events._sounddevice_input_channels(sd), 1)

    def test_channel_candidates_prefer_widest_then_2_then_1(self):
        self.assertEqual(vp_events._sounddevice_capture_channel_candidates(2), [2, 1])
        self.assertEqual(vp_events._sounddevice_capture_channel_candidates(12), [8, 2, 1])
        self.assertEqual(vp_events._sounddevice_capture_channel_candidates(1), [1])

    def test_stream_kwargs_try_low_latency_before_default(self):
        # Needs vp_transcribe.SR; importing it requires numpy stubbed by helpers.
        real_numpy()
        cb = object()
        low_latency, fallback = vp_events._sounddevice_stream_kwargs(2, cb)
        self.assertEqual(low_latency["latency"], "low")
        self.assertIn("blocksize", low_latency)
        self.assertNotIn("latency", fallback)
        self.assertEqual(fallback["channels"], 2)
        self.assertIs(fallback["callback"], cb)


class AudioMeterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.np = real_numpy()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def test_meter_level_clamps_and_handles_nan(self):
        self.assertEqual(vp_events._audio_meter_level_from_dbfs(float("nan")), 0.0)
        self.assertEqual(vp_events._audio_meter_level_from_dbfs(-120.0), 0.0)
        self.assertAlmostEqual(vp_events._audio_meter_level_from_dbfs(-12.0), 1.0, places=6)
        self.assertEqual(vp_events._audio_meter_level_from_dbfs("not-a-number"), 0.0)

    def test_select_active_channel_picks_loudest(self):
        np = self.np
        quiet = np.zeros(1000, dtype=np.int16)
        loud = np.full(1000, 5000, dtype=np.int16)
        stereo = np.stack([quiet, loud], axis=1)
        mono = vp_events._select_active_channel_pcm(stereo)
        self.assertEqual(mono.shape, (1000, 1))
        self.assertEqual(int(mono[0, 0]), 5000)

    def test_audio_level_metrics_use_rms_for_meter(self):
        np = self.np
        pcm = (np.full((2000, 1), 0.1, dtype=np.float32) * 32767).astype(np.int16)
        raw_dbfs, peak, level = vp_events._audio_level_metrics(pcm)
        self.assertAlmostEqual(raw_dbfs, -20.0, places=0)
        self.assertGreater(level, 0.7)
        self.assertGreater(peak, 0.0)

    def test_audio_level_metrics_empty_is_floor(self):
        np = self.np
        raw_dbfs, peak, level = vp_events._audio_level_metrics(
            np.zeros((0, 1), dtype=np.int16))
        self.assertEqual((raw_dbfs, peak, level), (-120.0, 0.0, 0.0))


class CommandHookTests(unittest.TestCase):
    def test_command_hook_annotates_event_from_helper_result(self):
        result = {
            "enabled": True, "command": "notify", "returncode": 0,
            "latency_ms": 12, "timeout": False, "error": None,
        }
        event = {"event": "utterance", "text": "hi"}
        with patch.object(vp_events, "_rust_json", lambda *a, **k: result):
            vp_events._run_command_hook_and_annotate(event)
        self.assertTrue(event["command_hook_enabled"])
        self.assertEqual(event["command_hook_command"], "notify")
        self.assertEqual(event["command_hook_latency_ms"], 12)

    def test_command_hook_defaults_when_helper_absent(self):
        event = {"event": "utterance"}
        with patch.object(vp_events, "_rust_json", lambda *a, **k: None):
            vp_events._run_command_hook_and_annotate(event)
        self.assertFalse(event["command_hook_enabled"])
        self.assertIsNone(event["command_hook_command"])
        self.assertEqual(event["command_hook_latency_ms"], 0)


class ApplyProfileTests(unittest.TestCase):
    def test_apply_profile_returns_base_when_no_helper(self):
        base = {"lang": "da"}
        with patch.object(vp_events, "_rust_json", lambda *a, **k: None):
            cfg, name = vp_events._apply_profile_settings(
                base, [], title="t", process="p")
        self.assertEqual(cfg, base)
        self.assertIsNot(cfg, base)  # defensive copy
        self.assertIsNone(name)

    def test_apply_profile_uses_helper_config_and_name(self):
        helper = {"config": {"lang": "en", "beam_size": 2}, "name": "Editor"}
        with patch.object(vp_events, "_rust_json", lambda *a, **k: helper):
            cfg, name = vp_events._apply_profile_settings(
                {"lang": "da"}, [{"name": "Editor"}], title="t", process="p")
        self.assertEqual(cfg, {"lang": "en", "beam_size": "2"})  # str-coerced
        self.assertEqual(name, "Editor")


class ModelCapacityTests(unittest.TestCase):
    def test_returns_false_without_helper(self):
        with _env(VOICEPI_RUST_INJECTOR=None):
            self.assertFalse(vp_events._print_model_capacity(as_json=False))

    def test_prints_helper_stdout_on_success(self):
        completed = types.SimpleNamespace(returncode=0, stdout="cap: ok\n", stderr="")
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_events.subprocess, "run", lambda *a, **k: completed), \
                _capture_stdout() as buf:
            ok = vp_events._print_model_capacity(as_json=True)
        self.assertTrue(ok)
        self.assertEqual(buf.getvalue().strip(), "cap: ok")

    def test_reports_helper_failure_to_stderr(self):
        completed = types.SimpleNamespace(returncode=1, stdout="", stderr="boom")
        with _env(VOICEPI_RUST_INJECTOR="whisper-dictate"), \
                patch.object(vp_events.subprocess, "run", lambda *a, **k: completed):
            stderr = io.StringIO()
            with redirect_stderr(stderr):
                ok = vp_events._print_model_capacity(as_json=False)
        self.assertFalse(ok)
        self.assertIn("boom", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
