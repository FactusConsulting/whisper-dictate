"""Tests for the ``--record-corpus-item`` worker mode (vp_corpus_record).

The mode records reference audio for a golden-corpus item from the configured
mic, REUSING the live-capture machinery, and saves a 16k mono 16-bit WAV to
``<appdata>/benchmark/audio/<id>.wav``. These tests drive
``vp_corpus_record.record_corpus_item`` with a tiny temp corpus and a FAKE
capture (frames injected directly — no real audio stack touched), asserting:

  * a duration heuristic that clamps short/long reference text + adds the lead-in,
  * an unknown id / missing corpus → a single ``corpus_record_error`` event, exit 0,
  * the WAV writer produces 16k mono 16-bit PCM at the expected appdata path,
  * native-rate frames are resampled to 16k via the REUSED capture helper,
  * corpus resolution reuses the benchmark path helpers (app-root → appdata),
  * the start/done event contract (text, seconds, path, peak/rms).

Real numpy is loaded (heavy deps stubbed) because the recorder concatenates and
resamples real int16 buffers.
"""
import json
import wave
from pathlib import Path

from helpers import _capture_stdout, load_voice_pi_realnp, patch, unittest


def _write_corpus(root: Path, items) -> Path:
    """Write a minimal ``benchmark/corpus.json`` under ``root`` and return root."""
    manifest = root / "benchmark" / "corpus.json"
    manifest.parent.mkdir(parents=True, exist_ok=True)
    manifest.write_text(json.dumps({
        "version": 1,
        "audio_dir": "audio",
        "items": items,
    }), encoding="utf-8")
    return root


def _events(stdout: str) -> list:
    """Parse every JSON line printed by the recorder into dicts."""
    out = []
    for line in stdout.splitlines():
        line = line.strip()
        if line.startswith("{"):
            out.append(json.loads(line))
    return out


class _RecorderTestBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            load_voice_pi_realnp()
        except ImportError as e:  # numpy missing
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import importlib

        cls.mod = importlib.import_module("whisper_dictate.vp_corpus_record")
        cls.vp_capture = importlib.import_module("whisper_dictate.vp_capture")
        cls.vp_capture._load_runtime_modules()
        import numpy as np

        cls.np = np


class DurationHeuristicTests(_RecorderTestBase):
    def test_short_text_clamps_to_minimum_plus_lead_in(self):
        # A tiny string would be < the 8s floor; it clamps up, then +2s lead-in.
        seconds = self.mod.compute_record_seconds("Hi")
        self.assertEqual(seconds, 8.0 + 2.0)

    def test_long_text_clamps_to_maximum_plus_lead_in(self):
        seconds = self.mod.compute_record_seconds("x" * 5000)
        self.assertEqual(seconds, 90.0 + 2.0)

    def test_mid_length_text_uses_chars_over_twelve(self):
        # 240 chars / 12 = 20s (within [8, 90]) + 2s lead-in = 22s.
        seconds = self.mod.compute_record_seconds("x" * 240)
        self.assertEqual(seconds, 20.0 + 2.0)

    def test_empty_text_still_gets_minimum_window(self):
        self.assertEqual(self.mod.compute_record_seconds(""), 8.0 + 2.0)


class ErrorEventTests(_RecorderTestBase):
    def test_unknown_id_reports_error_event_and_exits_zero(self):
        root = _write_corpus(Path(self._tmp()), [
            {"id": "da-001", "language": "da", "text": "Hej med dig.", "terms": []},
        ])
        with _capture_stdout() as out:
            code = self.mod.record_corpus_item(
                "does-not-exist", app_root=str(root), appdata=str(root))
        self.assertEqual(code, 0)
        events = _events(out.getvalue())
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["event"], "corpus_record_error")
        self.assertIn("unknown corpus id", events[0]["error"])

    def test_missing_corpus_reports_error_event_and_exits_zero(self):
        empty = Path(self._tmp())  # no benchmark/corpus.json under here
        with _capture_stdout() as out:
            code = self.mod.record_corpus_item(
                "da-001", app_root=str(empty), appdata=str(empty))
        self.assertEqual(code, 0)
        events = _events(out.getvalue())
        self.assertEqual(events[0]["event"], "corpus_record_error")
        self.assertIn("no benchmark corpus", events[0]["error"])

    def test_capture_failure_reports_error_event_not_raises(self):
        # A capture that blows up (e.g. device gone) must surface as a clean
        # error event with exit 0, never raise out of the worker mode.
        root = _write_corpus(Path(self._tmp()), [
            {"id": "da-001", "language": "da", "text": "Hej med dig.", "terms": []},
        ])

        def _boom(_rec):
            raise RuntimeError("microphone exploded")

        with patch.object(self.mod, "_start_capture", _boom):
            with _capture_stdout() as out:
                code = self.mod.record_corpus_item(
                    "da-001", app_root=str(root), appdata=str(root))
        self.assertEqual(code, 0)
        events = _events(out.getvalue())
        # start event prints before capture; the error event follows it.
        self.assertEqual(events[0]["event"], "corpus_record_start")
        self.assertEqual(events[-1]["event"], "corpus_record_error")
        self.assertIn("microphone exploded", events[-1]["error"])

    def _tmp(self) -> str:
        import tempfile

        d = tempfile.mkdtemp(prefix="wd-corpus-rec-")
        self.addCleanup(self._rmtree, d)
        return d

    @staticmethod
    def _rmtree(path: str) -> None:
        import shutil

        shutil.rmtree(path, ignore_errors=True)


class WavWriterAndContractTests(_RecorderTestBase):
    def _tmp(self) -> str:
        import tempfile

        d = tempfile.mkdtemp(prefix="wd-corpus-rec-")
        self.addCleanup(self._rmtree, d)
        return d

    @staticmethod
    def _rmtree(path: str) -> None:
        import shutil

        shutil.rmtree(path, ignore_errors=True)

    def _record_with_fake_frames(self, frames, *, capture_rate, text="Hej med dig."):
        """Run record_corpus_item with capture stubbed to inject ``frames``.

        Patches ``_start_capture`` (sets the chosen rate) and ``_collect_for``
        (no real waiting) so the recorder's resample/WAV/event path runs against
        the injected buffer with no audio stack involved.
        """
        root = _write_corpus(Path(self._tmp()), [
            {"id": "da-001", "language": "da", "text": text, "terms": []},
        ])

        def _fake_start(rec):
            rec._capture_rate = capture_rate
            rec.frames = list(frames)

        with patch.object(self.mod, "_start_capture", _fake_start), \
                patch.object(self.mod, "_collect_for", lambda *_a: None):
            with _capture_stdout() as out:
                code = self.mod.record_corpus_item(
                    "da-001", app_root=str(root), appdata=str(root))
        return code, _events(out.getvalue()), root

    def test_writes_16k_mono_16bit_wav_at_appdata_path(self):
        np = self.np
        # 16000 native-rate int16 frames (already 16k → no resample needed).
        frame = np.full((16000, 1), 1000, dtype=np.int16)
        code, events, root = self._record_with_fake_frames(
            [frame], capture_rate=16000)

        self.assertEqual(code, 0)
        done = events[-1]
        self.assertEqual(done["event"], "corpus_record_done")
        self.assertEqual(done["id"], "da-001")
        expected = root / "benchmark" / "audio" / "da-001.wav"
        self.assertEqual(Path(done["path"]), expected)
        self.assertTrue(expected.exists())

        with wave.open(str(expected), "rb") as wav:
            self.assertEqual(wav.getnchannels(), 1)
            self.assertEqual(wav.getsampwidth(), 2)  # 16-bit
            self.assertEqual(wav.getframerate(), 16000)
            self.assertEqual(wav.getnframes(), 16000)

    def test_native_48k_frames_are_resampled_to_16k_via_capture_helper(self):
        np = self.np
        # 48000 samples at 48k == 1.0s → resampled to 16000 samples at 16k.
        frame = np.full((48000, 1), 500, dtype=np.int16)

        called = {}
        real_resample = self.vp_capture._resample_capture_buffer

        def _spy(pcm, rate):
            called["rate"] = rate
            return real_resample(pcm, rate)

        with patch.object(self.vp_capture, "_resample_capture_buffer", _spy):
            code, events, root = self._record_with_fake_frames(
                [frame], capture_rate=48000)

        self.assertEqual(code, 0)
        # The REUSED capture resample helper was driven with the native rate.
        self.assertEqual(called["rate"], 48000)
        done = events[-1]
        # ~1.0s of audio at 16k after the 48k→16k downsample.
        self.assertAlmostEqual(done["seconds_recorded"], 1.0, places=1)
        with wave.open(str(root / "benchmark" / "audio" / "da-001.wav"), "rb") as wav:
            self.assertEqual(wav.getframerate(), 16000)
            self.assertAlmostEqual(wav.getnframes(), 16000, delta=2)

    def test_start_event_carries_reference_text_and_seconds(self):
        np = self.np
        frame = np.full((16000, 1), 800, dtype=np.int16)
        _code, events, _root = self._record_with_fake_frames(
            [frame], capture_rate=16000, text="Læs denne sætning højt.")
        start = events[0]
        self.assertEqual(start["event"], "corpus_record_start")
        self.assertEqual(start["id"], "da-001")
        self.assertEqual(start["text"], "Læs denne sætning højt.")
        self.assertEqual(
            start["seconds"], self.mod.compute_record_seconds(start["text"]))

    def test_done_event_reports_peak_and_rms_dbfs(self):
        np = self.np
        frame = np.full((16000, 1), 16384, dtype=np.int16)  # ~ -6 dBFS peak
        _code, events, _root = self._record_with_fake_frames(
            [frame], capture_rate=16000)
        done = events[-1]
        self.assertIn("peak_dbfs", done)
        self.assertIn("rms_dbfs", done)
        # 16384/32768 == 0.5 → ~ -6 dBFS peak.
        self.assertAlmostEqual(done["peak_dbfs"], -6.0, delta=0.5)

    def test_no_audio_captured_reports_error(self):
        # Empty frames → a clear error rather than an empty WAV.
        code, events, _root = self._record_with_fake_frames([], capture_rate=16000)
        self.assertEqual(code, 0)
        self.assertEqual(events[-1]["event"], "corpus_record_error")
        self.assertIn("no audio", events[-1]["error"])


class CliFlagTests(unittest.TestCase):
    def test_parser_exposes_record_corpus_item_flag(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args(["--record-corpus-item", "da-001"])
        self.assertEqual(args.record_corpus_item, "da-001")

    def test_flag_defaults_none(self):
        from whisper_dictate.vp_cli import build_arg_parser

        args = build_arg_parser().parse_args([])
        self.assertIsNone(args.record_corpus_item)


if __name__ == "__main__":
    unittest.main()
