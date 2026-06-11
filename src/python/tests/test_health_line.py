"""Unit tests for the per-utterance ``[health]`` line (Basic diagnostics)."""
from __future__ import annotations

import os
import sys
import unittest
from contextlib import contextmanager
from pathlib import Path

sys.path.insert(0, str(Path("src/python")))

from whisper_dictate import runtime  # noqa: E402
from whisper_dictate.vp_health import format_health_line  # noqa: E402


@contextmanager
def _env(**values):
    saved = {k: os.environ.get(k) for k in values}
    try:
        for k, v in values.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        yield
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class ConfigDumpGatingTests(unittest.TestCase):
    """The startup config dump moved from Basic to Verbose: it now requires
    BOTH VOICEPI_DEBUG and VOICEPI_STT_DEBUG (Verbose), not debug alone (Basic).
    """

    def test_dump_off_at_off_level(self):
        with _env(VOICEPI_DEBUG=None, VOICEPI_STT_DEBUG=None):
            self.assertFalse(runtime._config_dump_enabled())

    def test_dump_off_at_basic_level(self):
        # Basic = debug:on, stt_debug:off -> NO config dump (health line instead)
        with _env(VOICEPI_DEBUG="1", VOICEPI_STT_DEBUG=None):
            self.assertFalse(runtime._config_dump_enabled())

    def test_dump_on_at_verbose_level(self):
        # Verbose = debug:on, stt_debug:on -> config dump prints
        with _env(VOICEPI_DEBUG="1", VOICEPI_STT_DEBUG="1"):
            self.assertTrue(runtime._config_dump_enabled())


def _segments(*logprobs: float) -> list[dict]:
    return [{"avg_logprob": lp} for lp in logprobs]


class HealthLineConfidenceBandTests(unittest.TestCase):
    def _base(self, **over) -> dict:
        metrics = {
            "audio_raw_dbfs": -38.0,
            "audio_snr_db": 56.0,
            "audio_input_status": "good",
            "post_mode": "clean",
            "post_processor": "groq",
        }
        metrics.update(over)
        return metrics

    def test_high_band(self):
        line = format_health_line(self._base(segments=_segments(-0.13, -0.20)))
        self.assertIn("confidence high (-0.17)", line)
        self.assertNotIn("WARN", line)

    def test_ok_band(self):
        line = format_health_line(self._base(segments=_segments(-0.45, -0.50)))
        self.assertIn("confidence ok (-0.47)", line)
        self.assertNotIn("WARN", line)

    def test_low_band_warns(self):
        line = format_health_line(self._base(segments=_segments(-0.80, -0.90)))
        self.assertIn("confidence low (-0.85)", line)
        self.assertIn("WARN low confidence", line)

    def test_band_boundaries(self):
        # -0.35 is the high/ok boundary (>= -0.35 => high)
        self.assertIn("high", format_health_line(self._base(segments=_segments(-0.35))))
        self.assertIn("ok", format_health_line(self._base(segments=_segments(-0.36))))
        # -0.60 is the ok/low boundary (>= -0.60 => ok)
        self.assertIn("ok", format_health_line(self._base(segments=_segments(-0.60))))
        self.assertIn("low", format_health_line(self._base(segments=_segments(-0.61))))

    def test_missing_segments_is_na(self):
        line = format_health_line(self._base(segments=[]))
        self.assertIn("confidence n/a", line)
        self.assertNotIn("(", line.split("confidence", 1)[1].split("|", 1)[0])


class HealthLineMicTests(unittest.TestCase):
    def test_good_input(self):
        line = format_health_line({
            "audio_raw_dbfs": -38.2,
            "audio_snr_db": 56.4,
            "audio_input_status": "good",
            "segments": _segments(-0.1),
        })
        self.assertTrue(line.startswith("[health] mic -38dBFS SNR 56dB good |"))

    def test_quiet_with_boost(self):
        line = format_health_line({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "audio_gain": 11.3,
            "segments": _segments(-0.2),
        })
        self.assertIn("quiet (boosted 11x)", line)
        # quiet + clean (high SNR) must NOT warn — that's the user's own setup.
        self.assertNotIn("WARN quiet input", line)

    def test_quiet_without_gain_has_no_boost_suffix(self):
        line = format_health_line({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("mic -44dBFS SNR 40dB quiet", line)
        self.assertNotIn("boosted", line)


class HealthLineWarnTests(unittest.TestCase):
    def test_quiet_and_low_snr_warns(self):
        line = format_health_line({
            "audio_raw_dbfs": -55.0,
            "audio_snr_db": 3.0,
            "audio_input_status": "too_quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("WARN quiet input", line)

    def test_quiet_status_with_low_snr_number_warns(self):
        # status="quiet" (level low) AND a low SNR number -> "we're off".
        line = format_health_line({
            "audio_raw_dbfs": -50.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "quiet",
            "segments": _segments(-0.2),
        })
        self.assertIn("WARN quiet input", line)

    def test_loud_but_low_snr_does_not_warn_quiet(self):
        # A loud-enough but noisy mic ("low_snr" status) is not "quiet input".
        line = format_health_line({
            "audio_raw_dbfs": -20.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "low_snr",
            "segments": _segments(-0.2),
        })
        self.assertNotIn("WARN quiet input", line)

    def test_no_text_warns(self):
        line = format_health_line({"no_text": True})
        self.assertIn("WARN no text", line)


class HealthLinePostTests(unittest.TestCase):
    def test_post_on(self):
        line = format_health_line({
            "post_mode": "clean",
            "post_processor": "groq",
            "segments": _segments(-0.1),
        })
        self.assertIn("post clean/groq", line)

    def test_post_off_when_none(self):
        line = format_health_line({
            "post_mode": "raw",
            "post_processor": "none",
            "segments": _segments(-0.1),
        })
        self.assertIn("post off", line)

    def test_post_off_when_missing(self):
        line = format_health_line({"segments": _segments(-0.1)})
        self.assertIn("post off", line)


if __name__ == "__main__":
    unittest.main()
