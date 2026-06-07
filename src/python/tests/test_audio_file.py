"""Unit tests for vp_audio_file (extracted from runtime.py).

Decode + calibration helpers that operate on a recorded buffer/file. The WAV
happy path and transcribe_file_event are already exercised in
test_dictionary_benchmark_history.py; these add the DSP edge cases
(_resample_mono / _mono_float_to_int16), the calibration status thresholds, the
missing-file guard, and the ffmpeg-required error path.
"""
import importlib
import sys
import tempfile
import wave

from helpers import (
    _capture_stdout,
    json,
    patch,
    real_numpy,
    unittest,
)


def _load_audio_file_module():
    """Import vp_audio_file (+ its numpy deps) against the REAL numpy."""
    numpy = real_numpy()
    sys.modules["numpy"] = numpy
    for name in ("whisper_dictate.vp_audio", "whisper_dictate.vp_transcribe",
                 "whisper_dictate.vp_audio_file"):
        sys.modules.pop(name, None)
    return importlib.import_module("whisper_dictate.vp_audio_file"), numpy


class CalibrationStatusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.mod, cls.np = _load_audio_file_module()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def test_clean_audio_passes(self):
        self.assertEqual(self.mod._calibration_status(-20.0, 40.0), ("pass", []))

    def test_marginal_audio_warns(self):
        status, warnings = self.mod._calibration_status(-45.0, 10.0)
        self.assertEqual(status, "warn")
        self.assertTrue(warnings)

    def test_silent_or_no_contrast_fails(self):
        status, warnings = self.mod._calibration_status(-60.0, 3.0)
        self.assertEqual(status, "fail")
        self.assertTrue(warnings)

    def test_analyze_calibration_emits_recommendations(self):
        np = self.np
        quiet = np.full(4000, 0.001, dtype=np.float32)
        speech = np.full(12000, 0.15, dtype=np.float32)
        pcm = (np.concatenate([quiet, speech]) * 32767).astype(np.int16)
        result = self.mod.analyze_calibration_audio(pcm)
        self.assertEqual(result["event"], "mic_calibration")
        self.assertEqual(result["status"], "pass")
        self.assertIn("VOICEPI_MIN_INPUT_DBFS", result["recommended"])

    def test_print_calibration_json_is_single_object(self):
        result = {
            "event": "mic_calibration", "status": "pass", "warnings": [],
            "raw_dbfs": -20.0, "noise_dbfs": -60.0, "snr_db": 40.0,
            "peak": 0.5, "recommended": {},
        }
        with _capture_stdout() as buf:
            self.mod.print_calibration_result(result, as_json=True)
        self.assertEqual(json.loads(buf.getvalue()), result)


class DecodeDspTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.mod, cls.np = _load_audio_file_module()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def test_mono_float_to_int16_clips_and_shapes(self):
        np = self.np
        audio = np.array([0.0, 1.0, -1.0, 2.0, -2.0], dtype=np.float32)
        out = self.mod._mono_float_to_int16(audio)
        self.assertEqual(out.dtype.name, "int16")
        self.assertEqual(out.shape, (5, 1))
        self.assertEqual(int(out[3, 0]), 32767)   # 2.0 clipped to +1.0
        self.assertEqual(int(out[4, 0]), -32767)  # -2.0 clipped to -1.0

    def test_resample_mono_is_identity_at_target_rate(self):
        np = self.np
        audio = np.linspace(-0.5, 0.5, num=100, dtype=np.float32)
        out = self.mod._resample_mono(audio, 16000)
        self.assertEqual(out.dtype.name, "float32")
        self.assertEqual(len(out), len(audio))

    def test_resample_mono_changes_length_for_other_rate(self):
        np = self.np
        audio = np.zeros(8000, dtype=np.float32)  # 1.0 s at 8 kHz
        out = self.mod._resample_mono(audio, 8000)
        self.assertEqual(len(out), 16000)  # upsampled to 16 kHz

    def test_resample_mono_handles_empty(self):
        np = self.np
        out = self.mod._resample_mono(np.zeros(0, dtype=np.float32), 8000)
        self.assertEqual(len(out), 0)


class LoadAudioFileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            cls.mod, cls.np = _load_audio_file_module()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")

    def _write_wav(self, path, rate=8000, seconds=0.8):
        import math
        import struct
        frames = int(rate * seconds)
        pcm = b"".join(
            struct.pack("<h", int(0.25 * 32767 * math.sin(2 * math.pi * 440 * i / rate)))
            for i in range(frames))
        with wave.open(path, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(rate)
            wav.writeframes(pcm)

    def test_load_wav_resamples_to_16k_mono_int16(self):
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_wav(path, rate=8000)
            pcm = self.mod.load_audio_file(path)
        finally:
            import os
            os.remove(path)
        self.assertEqual(pcm.dtype.name, "int16")
        self.assertEqual(pcm.shape[1], 1)
        self.assertGreaterEqual(len(pcm), 12000)  # 0.8s * 16k ~= 12800

    def test_missing_file_raises(self):
        with self.assertRaises(FileNotFoundError):
            self.mod.load_audio_file("does-not-exist-xyz.wav")

    def test_non_wav_without_ffmpeg_raises_helpful_error(self):
        # Force the ffmpeg path and simulate ffmpeg not installed.
        with tempfile.NamedTemporaryFile(suffix=".mp3", delete=False) as f:
            path = f.name
        try:
            with patch.object(self.mod.subprocess, "run",
                              side_effect=FileNotFoundError()):
                with self.assertRaisesRegex(RuntimeError, "ffmpeg"):
                    self.mod.load_audio_file(path)
        finally:
            import os
            os.remove(path)

    def test_print_transcribe_file_result_text_vs_json(self):
        event = {"text": "hi there", "event": "file_transcription"}
        with _capture_stdout() as buf:
            self.mod.print_transcribe_file_result(event, as_json=False)
        self.assertEqual(buf.getvalue().strip(), "hi there")
        with _capture_stdout() as buf:
            self.mod.print_transcribe_file_result(event, as_json=True)
        self.assertEqual(json.loads(buf.getvalue()), event)


if __name__ == "__main__":
    unittest.main()
